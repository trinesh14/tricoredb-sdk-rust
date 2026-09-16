//! TLS, built on rustls. Enabled by the crate's default `tls` feature.
//!
//! Two rules this module keeps, because both are easy to get wrong in a way
//! nothing visibly fails on:
//!
//! * With no CA file the trust store stays **empty**. It never falls back to
//!   the operating system's roots, so a wrong path fails closed.
//! * Errors name a certificate or key file's **path**, never its contents.

use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
};

use crate::error::{Error, ErrorKind, Result};
use crate::options::TlsOptions;
use crate::transport::Transport;

fn tls_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Tls, message)
}

/// Wrap an open TCP connection in TLS and complete the handshake.
pub(crate) fn wrap(tcp: TcpStream, opts: &TlsOptions) -> Result<Transport> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_error(format!("cannot configure TLS: {e}")))?;

    let verifier_stage = if opts.danger_accept_invalid_certs {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServer {
                supported: provider.signature_verification_algorithms,
            }))
    } else {
        builder.with_root_certificates(root_store(opts.ca_file.as_deref())?)
    };

    let config =
        match (&opts.client_cert_file, &opts.client_key_file) {
            (Some(cert), Some(key)) => {
                let chain = load_certs(cert)?;
                let key = load_key(key)?;
                verifier_stage
                    .with_client_auth_cert(chain, key)
                    .map_err(|e| {
                        tls_error(format!(
                            "client certificate {} with key {}: {e}",
                            cert.display(),
                            opts.client_key_file
                                .as_deref()
                                .unwrap_or(Path::new(""))
                                .display()
                        ))
                    })?
            }
            (None, None) => verifier_stage.with_no_client_auth(),
            (Some(_), None) => return Err(Error::invalid(
                "client_key_file is required alongside client_cert_file (mutual TLS needs both)",
            )),
            (None, Some(_)) => return Err(Error::invalid(
                "client_cert_file is required alongside client_key_file (mutual TLS needs both)",
            )),
        };

    let name = opts
        .server_name
        .clone()
        .unwrap_or_else(|| "localhost".into());
    let server_name = ServerName::try_from(name.clone())
        .map_err(|e| tls_error(format!("`{name}` is not a valid server name: {e}")))?;
    let connection = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| tls_error(format!("cannot start a TLS session with `{name}`: {e}")))?;

    // Complete the handshake here rather than on the first request, so a
    // refused certificate is reported by `connect` and while the connect
    // deadline still applies.
    let mut stream = StreamOwned::new(connection, tcp);
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(|e| tls_error(format!("TLS handshake with `{name}` failed: {e}")))?;
    }
    Ok(Transport::Tls(Box::new(stream)))
}

/// The trust store: empty, plus whatever the CA file holds.
fn root_store(ca_file: Option<&Path>) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    let Some(path) = ca_file else {
        return Ok(roots);
    };
    let certs = load_certs(path)?;
    if certs.is_empty() {
        return Err(tls_error(format!(
            "CA file {} holds no PEM certificates",
            path.display()
        )));
    }
    for cert in certs {
        roots
            .add(cert)
            .map_err(|e| tls_error(format!("CA file {}: {e}", path.display())))?;
    }
    Ok(roots)
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_file_iter(path)
        .map_err(|e| tls_error(format!("certificate file {}: {e}", path.display())))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| tls_error(format!("certificate file {}: {e}", path.display())))
}

fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    // The error carries the path only. A parse failure must never quote the
    // bytes it failed on: those bytes are the private key.
    PrivateKeyDer::from_pem_file(path)
        .map_err(|e| tls_error(format!("private key file {}: {e}", path.display())))
}

/// The verifier behind [`TlsOptions::danger_accept_invalid_certs`]: it accepts
/// every certificate, so the session is encrypted but authenticates nobody.
#[derive(Debug)]
struct AcceptAnyServer {
    supported: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AcceptAnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.supported)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.supported)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ca_file_leaves_the_trust_store_empty() {
        assert_eq!(root_store(None).unwrap().len(), 0);
    }

    #[test]
    fn a_missing_ca_file_is_a_tls_error_naming_the_path() {
        let err = root_store(Some(Path::new("no/such/ca.pem"))).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Tls);
        assert!(err.message.contains("no/such/ca.pem"), "{}", err.message);
    }

    #[test]
    fn half_an_identity_is_refused_before_connecting() {
        let opts = TlsOptions::new();
        let mut only_cert = opts.clone();
        only_cert.client_cert_file = Some("cert.pem".into());
        let err = wrap(dummy_socket(), &only_cert).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidArgument);
        assert!(err.message.contains("client_key_file"), "{}", err.message);

        let mut only_key = opts;
        only_key.client_key_file = Some("key.pem".into());
        let err = wrap(dummy_socket(), &only_key).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidArgument);
        assert!(err.message.contains("client_cert_file"), "{}", err.message);
    }

    /// A socket that is never written to: the identity check above happens
    /// before the handshake starts.
    fn dummy_socket() -> TcpStream {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        drop(listener);
        client
    }
}
