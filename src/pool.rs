//! A bounded pool of connections, for work that runs on several threads.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::client::Client;
use crate::error::{Error, ErrorKind, Result};
use crate::options::Options;

/// A bounded, thread-safe pool of connections.
///
/// A [`Client`] is one request/response stream, so concurrency means one
/// connection per concurrent caller. Opening one per request costs a TCP
/// connect plus a handshake every time; a pool pays that once.
///
/// [`Pool::with_connection`] lends a connection for the duration of a closure.
/// That is a closure rather than a guard type on purpose: a guard is something
/// a caller can hold across threads, which is the sharing this type exists to
/// prevent.
///
/// ```no_run
/// # fn main() -> tricoredb::Result<()> {
/// use tricoredb::{Options, Pool};
/// let pool = Pool::new(Options::new("127.0.0.1", 8427).user("admin").secret("pw"), 8)?;
/// pool.with_connection(|db| {
///     db.execute("INSERT INTO t VALUES (1, 'ada')")?;
///     Ok(())
/// })?;
/// # Ok(()) }
/// ```
#[derive(Clone, Debug)]
pub struct Pool {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    options: Options,
    size: usize,
    state: Mutex<State>,
    free: Condvar,
}

#[derive(Debug)]
struct State {
    idle: Vec<Client>,
    lent: usize,
    closed: bool,
}

impl Pool {
    /// Build a pool holding at most `size` connections.
    ///
    /// Connections are opened as they are needed: a pool sized for peak load
    /// should not pay for peak load at startup.
    pub fn new(options: Options, size: usize) -> Result<Pool> {
        if size == 0 {
            return Err(Error::invalid(
                "a pool needs room for at least one connection",
            ));
        }
        Ok(Pool {
            inner: Arc::new(Inner {
                options,
                size,
                state: Mutex::new(State {
                    idle: Vec::new(),
                    lent: 0,
                    closed: false,
                }),
                free: Condvar::new(),
            }),
        })
    }

    /// How many connections the pool may hold.
    pub fn size(&self) -> usize {
        self.inner.size
    }

    /// How many connections are idle, and how many are lent out.
    pub fn stats(&self) -> (usize, usize) {
        let state = self.lock();
        (state.idle.len(), state.lent)
    }

    /// Whether [`Pool::close`] has run.
    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Borrow a connection for the duration of `work`, waiting for a free one.
    pub fn with_connection<T>(&self, work: impl FnOnce(&mut Client) -> Result<T>) -> Result<T> {
        self.run(None, work)
    }

    /// Borrow a connection, giving up after `timeout` rather than growing past
    /// the pool's size — an unbounded pool does not fix overload, it moves it
    /// to the server.
    pub fn with_connection_timeout<T>(
        &self,
        timeout: Duration,
        work: impl FnOnce(&mut Client) -> Result<T>,
    ) -> Result<T> {
        self.run(Some(Instant::now() + timeout), work)
    }

    fn run<T>(
        &self,
        deadline: Option<Instant>,
        work: impl FnOnce(&mut Client) -> Result<T>,
    ) -> Result<T> {
        let mut client = self.acquire(deadline)?;
        let outcome = work(&mut client);

        // A connection goes back only if the next borrower can trust it: a
        // broken stream, or a transaction this callback left open, must not.
        let mut broken = client.is_poisoned();
        if let Err(error) = &outcome {
            broken = broken || error.is_connection_fatal();
        }
        let mut left_open = false;
        if !broken && client.in_transaction() {
            left_open = true;
            // A rollback this client could not complete leaves a session whose
            // state no next borrower can assume anything about.
            broken = client.rollback().is_err();
        }
        self.release(client, broken);

        match outcome {
            Err(error) => Err(error),
            Ok(_) if left_open => Err(Error::invalid(
                "the closure returned with a transaction still open on the pooled connection; it \
has been rolled back rather than handed to the next borrower. Commit or roll back inside the \
closure, or use Client::with_transaction",
            )),
            Ok(value) => Ok(value),
        }
    }

    fn acquire(&self, deadline: Option<Instant>) -> Result<Client> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return Err(Error::new(ErrorKind::Pool, "this pool is closed"));
            }
            if let Some(client) = state.idle.pop() {
                state.lent += 1;
                return Ok(client);
            }
            if state.lent < self.inner.size {
                // Count the slot before connecting, so two threads cannot both
                // decide there is room for the last one.
                state.lent += 1;
                drop(state);
                return Client::connect(&self.inner.options).inspect_err(|_| {
                    let mut state = self.lock();
                    state.lent -= 1;
                    self.inner.free.notify_one();
                });
            }
            state = match deadline {
                None => self
                    .inner
                    .free
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner()),
                Some(deadline) => {
                    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                        return Err(Error::new(
                            ErrorKind::Pool,
                            "no pooled connection became free within the timeout",
                        ));
                    };
                    let (state, timed_out) = self
                        .inner
                        .free
                        .wait_timeout(state, remaining)
                        .unwrap_or_else(|e| e.into_inner());
                    if timed_out.timed_out()
                        && state.idle.is_empty()
                        && state.lent >= self.inner.size
                    {
                        return Err(Error::new(
                            ErrorKind::Pool,
                            "no pooled connection became free within the timeout",
                        ));
                    }
                    state
                }
            };
        }
    }

    fn release(&self, client: Client, broken: bool) {
        let mut state = self.lock();
        state.lent -= 1;
        if broken || state.closed {
            drop(state);
            let _ = client.close();
        } else {
            state.idle.push(client);
            drop(state);
        }
        self.inner.free.notify_one();
    }

    /// Close every idle connection and refuse new borrowing. Connections
    /// currently lent out close when they come back.
    pub fn close(&self) {
        let idle = {
            let mut state = self.lock();
            state.closed = true;
            std::mem::take(&mut state.idle)
        };
        for client in idle {
            let _ = client.close();
        }
        self.inner.free.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while a connection was lent out leaves the counts intact, so
        // the pool stays usable rather than poisoning every later borrow.
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_needs_room_for_one_connection() {
        let err = Pool::new(Options::default(), 0).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidArgument);
        assert!(Pool::new(Options::default(), 1).is_ok());
    }

    #[test]
    fn a_new_pool_holds_no_connections_yet() {
        let pool = Pool::new(Options::default(), 4).unwrap();
        assert_eq!(pool.size(), 4);
        assert_eq!(pool.stats(), (0, 0));
        assert!(!pool.is_closed());
    }

    #[test]
    fn a_closed_pool_refuses_to_lend() {
        let pool = Pool::new(Options::default(), 2).unwrap();
        pool.close();
        assert!(pool.is_closed());
        let err = pool.with_connection(|_| Ok(())).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Pool);
    }

    #[test]
    fn a_failed_connect_gives_the_slot_back() {
        // Nothing listens here, so every borrow fails to connect. The slot must
        // still be released, or the pool would shrink to nothing.
        let pool = Pool::new(Options::new("127.0.0.1", 1), 2).unwrap();
        for _ in 0..3 {
            assert!(pool.with_connection(|_| Ok(())).is_err());
            assert_eq!(pool.stats(), (0, 0));
        }
    }
}
