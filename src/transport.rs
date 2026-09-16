//! The socket under a [`crate::Client`]: plain TCP, or TCP inside TLS.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::error::{Error, ErrorKind, Result};

/// A connected stream, with or without TLS.
#[derive(Debug)]
pub(crate) enum Transport {
    Tcp(TcpStream),
    #[cfg(feature = "tls")]
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Transport {
    /// The underlying socket, for deadlines and shutdown.
    fn socket(&self) -> &TcpStream {
        match self {
            Transport::Tcp(s) => s,
            #[cfg(feature = "tls")]
            Transport::Tls(s) => s.get_ref(),
        }
    }

    /// Bound each read, or clear the bound with `None`.
    pub(crate) fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        // A zero duration is an error to the operating system, and means
        // "no deadline" to every caller in this crate.
        let timeout = timeout.filter(|d| !d.is_zero());
        self.socket()
            .set_read_timeout(timeout)
            .map_err(Error::from_io)
    }

    /// Bound each write the same way.
    pub(crate) fn set_write_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        let timeout = timeout.filter(|d| !d.is_zero());
        self.socket()
            .set_write_timeout(timeout)
            .map_err(Error::from_io)
    }

    /// Drop the connection in both directions, ignoring a socket that is
    /// already gone.
    pub(crate) fn shutdown(&self) {
        let _ = self.socket().shutdown(std::net::Shutdown::Both);
    }
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Transport::Tcp(s) => s.read(buf),
            #[cfg(feature = "tls")]
            Transport::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Transport::Tcp(s) => s.write(buf),
            #[cfg(feature = "tls")]
            Transport::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Transport::Tcp(s) => s.flush(),
            #[cfg(feature = "tls")]
            Transport::Tls(s) => s.flush(),
        }
    }
}

/// Open a TCP connection, honouring `connect_timeout` when one is set.
pub(crate) fn dial(host: &str, port: u16, connect_timeout: Option<Duration>) -> Result<TcpStream> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::new(ErrorKind::Io, format!("cannot resolve {host}:{port}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(Error::new(
            ErrorKind::Io,
            format!("{host}:{port} resolved to no addresses"),
        ));
    }

    let mut last: Option<io::Error> = None;
    for addr in &addrs {
        let attempt = match connect_timeout {
            Some(t) if !t.is_zero() => TcpStream::connect_timeout(addr, t),
            _ => TcpStream::connect(addr),
        };
        match attempt {
            Ok(stream) => {
                // Request/response round trips are latency-sensitive, not bulk
                // transfer: Nagle's algorithm would add delay to every one.
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    let e = last.expect("at least one address was tried");
    Err(Error::new(
        ErrorKind::Io,
        format!("cannot connect to {host}:{port}: {e}"),
    ))
}
