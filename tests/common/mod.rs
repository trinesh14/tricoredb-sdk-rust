//! Starting a real `tricore-server` for the tests that need one.
//!
//! The binary is named by `TRICORE_SERVER_BIN`, or found by walking up from the
//! crate to a `target/release` or `target/debug` build of the server. It is
//! never built here: building the server from a test is slow and collides with
//! anything else compiling.
//!
//! When no binary is found, the server-backed tests print why and pass. That is
//! deliberate: someone who installed this crate from crates.io has no server
//! binary, and `cargo test` must still be green for them.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tricoredb::{Client, Options};

/// Every module on, dev authentication, and an ephemeral port so parallel runs
/// do not collide.
const CONFIG: &str = r#"
[server]
host = "127.0.0.1"
port = 0
protocol = "tricore"
node_id = "sdk-rust-tests"
region_id = "local"

[modules]
sql = true
document = true
cache = true
vector = true
graph = true
llm = true
cluster = false

[security]
auth_mode = "password"
dev_auth = true
allow_default_admin = false

[tls]
enabled = false
"#;

/// A running server, stopped when this value is dropped.
pub struct Server {
    process: Child,
    pub host: String,
    pub port: u16,
    _data_dir: TempDir,
}

impl Server {
    /// Connect to this server as `admin`.
    pub fn client(&self) -> Client {
        Client::connect(&self.options()).expect("connect to the test server")
    }

    /// Options pointing at this server.
    pub fn options(&self) -> Options {
        Options::new(&self.host, self.port)
            .user("admin")
            .secret("pw")
            .connect_timeout(Duration::from_secs(15))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// A directory removed when this value is dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> std::io::Result<TempDir> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!("tricoredb-rust-{tag}-{stamp}"));
        std::fs::create_dir_all(&path)?;
        Ok(TempDir(path))
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Start a server, or say why none could be started.
///
/// The error is a plain string because every caller does the same thing with
/// it: print it and pass.
pub fn start_server() -> Result<Server, String> {
    let binary = find_server_binary()?;
    let dir = TempDir::new("server").map_err(|e| format!("cannot make a temp directory: {e}"))?;
    let config = dir.path().join("tricore.toml");
    std::fs::write(&config, CONFIG).map_err(|e| format!("cannot write the config: {e}"))?;
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("cannot make the data dir: {e}"))?;

    let mut process = Command::new(&binary)
        .arg("--config")
        .arg(&config)
        .arg("--port")
        .arg("0")
        .arg("--data-dir")
        .arg(&data_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", binary.display()))?;

    let stdout = process.stdout.take().expect("stdout was piped");
    let (sender, receiver) = mpsc::channel();
    // The port is read from the server's own "listening on" line rather than
    // assumed: sibling runs bind their own servers at the same time.
    //
    // The thread keeps draining stdout for the server's whole life. Stopping at
    // the address line closes the pipe's read end, and the server's next write
    // to a closed stdout takes the server down with it — which looks exactly
    // like a server that started and then refused every connection.
    std::thread::spawn(move || {
        let mut sender = Some(sender);
        for line in BufReader::new(stdout)
            .lines()
            .map_while(std::io::Result::ok)
        {
            if let Some(address) = line.split("listening on").nth(1) {
                if let Some(sender) = sender.take() {
                    let _ = sender.send(address.trim().to_string());
                }
            }
        }
    });

    let address = receiver
        .recv_timeout(Duration::from_secs(60))
        .map_err(|_| "the server never said which address it is listening on".to_string())?;
    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| format!("cannot read an address out of `{address}`"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("cannot read a port out of `{address}`"))?;

    let server = Server {
        process,
        host: host.to_string(),
        port,
        _data_dir: dir,
    };
    wait_until_accepting(&server.host, server.port)?;
    Ok(server)
}

fn wait_until_accepting(host: &str, port: u16) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if TcpStream::connect((host, port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("{host}:{port} never started accepting connections"))
}

fn find_server_binary() -> Result<PathBuf, String> {
    if let Ok(path) = std::env::var("TRICORE_SERVER_BIN") {
        let path = PathBuf::from(path);
        return if path.exists() {
            Ok(path)
        } else {
            Err(format!(
                "TRICORE_SERVER_BIN points at {}, which does not exist",
                path.display()
            ))
        };
    }
    let name = if cfg!(windows) {
        "tricore-server.exe"
    } else {
        "tricore-server"
    };
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        for profile in ["release", "debug"] {
            let candidate = dir.join("target").join(profile).join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        if !dir.pop() {
            return Err(format!(
                "no {name} found: set TRICORE_SERVER_BIN to one, or run a server from the \
Docker image (see the README)"
            ));
        }
    }
}

/// Run `test` against a fresh server, or print why it was skipped.
///
/// ```ignore
/// with_server("cache round trip", |db| { … });
/// ```
pub fn with_server(name: &str, test: impl FnOnce(&mut Client)) {
    match start_server() {
        Ok(server) => {
            let mut client = server.client();
            test(&mut client);
        }
        Err(reason) => eprintln!("skipping `{name}`: {reason}"),
    }
}

/// A name no other test in this run uses, so tests can share one server without
/// colliding.
pub fn unique(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "{prefix}_{stamp:x}_{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}
