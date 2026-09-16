//! What a connection is configured with.

use std::path::PathBuf;
use std::time::Duration;

/// The port `tricore-server` listens on unless it was configured otherwise.
pub const DEFAULT_PORT: u16 = 8427;

/// The database a request names when none was chosen.
pub const DEFAULT_DATABASE: &str = "main";

/// This crate's own version, so an application can report the client it uses.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The client understands a request-scoped correlation id that the server joins
/// to its access log, audit trail and cancel registry.
pub const FEATURE_CORRELATION_ID: u64 = 1 << 0;

/// The server binds `?` placeholders to a typed array sent beside the
/// statement, instead of the client rendering values into the SQL text.
pub const FEATURE_SERVER_PARAMS: u64 = 1 << 1;

/// `BEGIN`, the statements, and `COMMIT`/`ROLLBACK` as separate requests on one
/// connection, with a real rollback boundary between them.
pub const FEATURE_SESSION_TXN: u64 = 1 << 2;

/// Every capability this build understands, and what [`Options`] announces
/// unless [`Options::features`] narrows it.
pub const ALL_FEATURES: u64 = FEATURE_CORRELATION_ID | FEATURE_SERVER_PARAMS | FEATURE_SESSION_TXN;

/// How the TLS session is set up.
///
/// Supplying [`TlsOptions`] at all is what turns TLS on. Once on, the server
/// certificate is verified and its name checked unless
/// [`TlsOptions::danger_accept_invalid_certs`] says otherwise.
///
/// Errors name the *path* of a certificate or key file, never its contents, so
/// key material cannot reach a log through this crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsOptions {
    /// PEM bundle used to verify the server.
    ///
    /// When this is `None` the trust store stays **empty** rather than falling
    /// back to the operating system's roots, so a wrong path fails closed
    /// instead of quietly succeeding against some unrelated public CA.
    pub ca_file: Option<PathBuf>,

    /// The name expected in the server's certificate, also sent as SNI.
    /// Defaults to `"localhost"`.
    pub server_name: Option<String>,

    /// Turns off certificate and host name verification.
    ///
    /// **Development only.** Such a connection looks encrypted but authenticates
    /// nothing, so anyone on the path can sit in the middle undetected — which
    /// is worse than visibly using plain TCP.
    pub danger_accept_invalid_certs: bool,

    /// PEM client certificate chain to present (mutual TLS). Needs
    /// [`TlsOptions::client_key_file`].
    pub client_cert_file: Option<PathBuf>,

    /// PEM private key for [`TlsOptions::client_cert_file`].
    pub client_key_file: Option<PathBuf>,
}

impl TlsOptions {
    /// TLS with an empty trust store: every server certificate is refused until
    /// [`TlsOptions::ca_file`] names the CA that signed it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Verify the server against this PEM bundle.
    pub fn ca_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_file = Some(path.into());
        self
    }

    /// Expect this name in the server's certificate.
    pub fn server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }

    /// Present a client identity (mutual TLS). Both files are required.
    pub fn client_identity(
        mut self,
        cert_file: impl Into<PathBuf>,
        key_file: impl Into<PathBuf>,
    ) -> Self {
        self.client_cert_file = Some(cert_file.into());
        self.client_key_file = Some(key_file.into());
        self
    }

    /// Turn off every check. **Development only** — see the field's own docs.
    pub fn danger_accept_invalid_certs(mut self, yes: bool) -> Self {
        self.danger_accept_invalid_certs = yes;
        self
    }
}

/// What to connect to, and how.
///
/// ```
/// use tricoredb::Options;
/// let opts = Options::new("127.0.0.1", 8427).user("admin").secret("pw");
/// assert_eq!(opts.database, "main");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Server host. Defaults to `127.0.0.1`.
    pub host: String,
    /// Server port. Defaults to [`DEFAULT_PORT`].
    pub port: u16,
    /// Principal to authenticate as. `None` skips authentication, which only a
    /// server that allows anonymous sessions accepts.
    pub user: Option<String>,
    /// The password or token sent with [`Options::user`].
    pub secret: Option<String>,
    /// The database named in every request. Defaults to [`DEFAULT_DATABASE`].
    pub database: String,
    /// The name this client reports in the handshake.
    pub client_name: String,
    /// A bound on the TCP connect, and on the TLS handshake when TLS is on.
    /// `None` leaves it to the operating system.
    pub connect_timeout: Option<Duration>,
    /// A bound on each wait for a reply, applied after the handshake.
    ///
    /// `None` (the default) means no bound. A statement legitimately runs for as
    /// long as it runs, and the server's own `statement_timeout_ms` is unlimited
    /// by default, so any number chosen here would be a guess at a guarantee the
    /// server does not make. Use [`crate::Client::set_request_timeout`] to make
    /// the *server* stop instead of only ending the wait.
    pub read_timeout: Option<Duration>,
    /// TLS settings. `None` means plain TCP, on which the secret crosses the
    /// wire in the clear.
    pub tls: Option<TlsOptions>,
    /// The capability bitmap announced in the handshake. `None` means
    /// [`ALL_FEATURES`].
    ///
    /// The server grants only what was asked for, so masking a bit out here is
    /// how a caller opts out of one:
    ///
    /// ```
    /// # use tricoredb::{Options, ALL_FEATURES, FEATURE_SESSION_TXN};
    /// let opts = Options::default().features(ALL_FEATURES & !FEATURE_SESSION_TXN);
    /// ```
    ///
    /// An explicit `Some(0)` announces nothing, which is why this is an option
    /// rather than a plain `u64`.
    pub features: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            user: None,
            secret: None,
            database: DEFAULT_DATABASE.to_string(),
            client_name: concat!("tricoredb-rust/", env!("CARGO_PKG_VERSION")).to_string(),
            connect_timeout: None,
            read_timeout: None,
            tls: None,
            features: None,
        }
    }
}

impl Options {
    /// Options for one host and port, with every other field defaulted.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            ..Self::default()
        }
    }

    /// Authenticate as this principal.
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// The secret sent with the user name.
    pub fn secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = Some(secret.into());
        self
    }

    /// Name the database every request runs against.
    pub fn database(mut self, database: impl Into<String>) -> Self {
        self.database = database.into();
        self
    }

    /// Identify this application in the handshake.
    pub fn client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = name.into();
        self
    }

    /// Bound the connect (and the TLS handshake).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Bound each wait for a reply.
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = Some(timeout);
        self
    }

    /// Turn TLS on with these settings.
    pub fn tls(mut self, tls: TlsOptions) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Announce exactly these capabilities in the handshake.
    pub fn features(mut self, features: u64) -> Self {
        self.features = Some(features);
        self
    }

    pub(crate) fn announced_features(&self) -> u64 {
        self.features.unwrap_or(ALL_FEATURES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let o = Options::default();
        assert_eq!(o.host, "127.0.0.1");
        assert_eq!(o.port, 8427);
        assert_eq!(o.database, "main");
        assert!(o.client_name.starts_with("tricoredb-rust/"));
        assert_eq!(o.read_timeout, None);
        assert!(o.tls.is_none());
        assert_eq!(o.announced_features(), ALL_FEATURES);
    }

    #[test]
    fn announcing_nothing_differs_from_leaving_it_unset() {
        assert_eq!(Options::default().features(0).announced_features(), 0);
    }

    #[test]
    fn every_feature_has_its_own_bit() {
        assert_eq!(FEATURE_CORRELATION_ID, 1);
        assert_eq!(FEATURE_SERVER_PARAMS, 2);
        assert_eq!(FEATURE_SESSION_TXN, 4);
        assert_eq!(ALL_FEATURES, 7);
    }
}
