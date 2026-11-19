//! TLS: `ssl_mode` semantics and the rustls client configuration.
//!
//! | mode              | encrypts            | verifies the certificate          |
//! |-------------------|---------------------|-----------------------------------|
//! | `Disabled`        | no                  | -                                 |
//! | `Preferred`       | if the server can   | no                                |
//! | `Required`        | yes                 | no                                |
//! | `VerifyCa`        | yes                 | chain only, any host name         |
//! | `VerifyIdentity`  | yes                 | chain and host name               |
//!
//! Verification trusts only the `ca` file when one is given, otherwise the
//! operating system's trust store.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::SystemTime;

use rustls::ClientConfig;
use rustls::DigitallySignedStruct;
use rustls::RootCertStore;
use rustls::SignatureScheme;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::HandshakeSignatureValid;
use rustls::client::danger::ServerCertVerified;
use rustls::client::danger::ServerCertVerifier;
use rustls::crypto::CryptoProvider;
use rustls::crypto::ring;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::ServerName;
use rustls::pki_types::UnixTime;
use rustls::pki_types::pem::PemObject;

use crate::error::Error;
use crate::error::Result;

/// How the connection uses TLS; see the module docs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SslMode {
    Disabled,
    #[default]
    Preferred,
    Required,
    VerifyCa,
    VerifyIdentity,
}

impl SslMode {
    /// Parses the names used by MySQL clients, case-insensitively.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "disabled" => Some(Self::Disabled),
            "preferred" => Some(Self::Preferred),
            "required" => Some(Self::Required),
            "verify_ca" => Some(Self::VerifyCa),
            "verify_identity" => Some(Self::VerifyIdentity),
            _ => None,
        }
    }

    /// Whether the connection must fail rather than fall back to plain text.
    #[must_use]
    pub const fn requires_tls(self) -> bool {
        matches!(self, Self::Required | Self::VerifyCa | Self::VerifyIdentity)
    }

    #[must_use]
    pub const fn verifies(self) -> bool {
        matches!(self, Self::VerifyCa | Self::VerifyIdentity)
    }
}

/// TLS settings of a connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsOpts {
    pub mode: SslMode,
    /// PEM file with the certificate authorities to trust.
    pub ca: Option<PathBuf>,
    /// PEM client certificate chain, for servers that require one.
    pub cert: Option<PathBuf>,
    /// PEM private key of `cert`.
    pub key: Option<PathBuf>,
}

pub(crate) fn tls_error(err: impl fmt::Display) -> Error {
    Error::Tls(err.to_string())
}

/// Configurations built so far, the most recently used last. Building one
/// reads the PEM files and, to verify without `ca`, loads the operating
/// system's trust store; connections sharing one also resume TLS sessions
/// (rustls keeps them in the configuration).
static CONFIGS: Mutex<Vec<(ConfigKey, Arc<ClientConfig>)>> = Mutex::new(Vec::new());
/// Distinct configurations kept; a process usually needs one or two.
const CACHED_CONFIGS: usize = 8;

/// What a configuration is built from: the options and the state of each
/// file they name, so a file replaced or rewritten on disk (a renewed
/// certificate) gives a new configuration.
#[derive(PartialEq, Eq)]
struct ConfigKey {
    opts: TlsOpts,
    files: [Option<FileStamp>; 3],
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    /// Device, inode and status change time (seconds, nanoseconds).
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn of(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        Ok(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt as _;
                (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
            },
        })
    }
}

impl ConfigKey {
    /// `None` when a file cannot be read: building the configuration then
    /// reports why (or does not need it), and nothing is cached.
    fn of(opts: &TlsOpts) -> Option<Self> {
        let stamp = |path: &Option<PathBuf>| path.as_deref().map(FileStamp::of).transpose();
        Some(Self {
            opts: opts.clone(),
            files: [
                stamp(&opts.ca).ok()?,
                stamp(&opts.cert).ok()?,
                stamp(&opts.key).ok()?,
            ],
        })
    }
}

/// The rustls configuration for `opts`, built once per process while its
/// files stay the same.
pub(crate) fn client_config(opts: &TlsOpts) -> Result<Arc<ClientConfig>> {
    let Some(key) = ConfigKey::of(opts) else {
        return build_config(opts);
    };
    let cached = |cache: &mut Vec<(ConfigKey, Arc<ClientConfig>)>| {
        let at = cache.iter().position(|(k, _)| *k == key)?;
        let entry = cache.remove(at);
        let config = Arc::clone(&entry.1);
        cache.push(entry);
        Some(config)
    };
    if let Some(config) = cached(&mut CONFIGS.lock().unwrap_or_else(PoisonError::into_inner)) {
        return Ok(config);
    }
    // Built without the lock: loading the system's trust store is slow.
    let config = build_config(opts)?;
    let mut cache = CONFIGS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(built) = cached(&mut cache) {
        // Another thread built it meanwhile.
        return Ok(built);
    }
    if cache.len() >= CACHED_CONFIGS {
        cache.remove(0);
    }
    cache.push((key, Arc::clone(&config)));
    Ok(config)
}

fn build_config(opts: &TlsOpts) -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(ring::default_provider());
    let verifier = verifier(opts, &provider)?;
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let config = match (&opts.cert, &opts.key) {
        (Some(cert), Some(key)) => builder
            .with_client_auth_cert(load_certs(cert)?, load_key(key)?)
            .map_err(tls_error)?,
        (None, None) => builder.with_no_client_auth(),
        _ => {
            return Err(Error::Tls(
                "a client certificate needs both ssl_cert and ssl_key".to_owned(),
            ));
        }
    };
    Ok(Arc::new(config))
}

/// Server certificate check implementing `opts.mode`.
fn verifier(opts: &TlsOpts, provider: &Arc<CryptoProvider>) -> Result<Arc<dyn ServerCertVerifier>> {
    if !opts.mode.verifies() {
        return Ok(Arc::new(NoVerification(Arc::clone(provider))));
    }
    let full: Arc<dyn ServerCertVerifier> = match &opts.ca {
        Some(path) => {
            let roots = Arc::new(load_roots(path)?);
            WebPkiServerVerifier::builder_with_provider(roots, Arc::clone(provider))
                .build()
                .map_err(tls_error)?
        }
        None => Arc::new(
            rustls_platform_verifier::Verifier::new(Arc::clone(provider)).map_err(tls_error)?,
        ),
    };
    Ok(if opts.mode == SslMode::VerifyCa {
        Arc::new(AnyHostName(full))
    } else {
        full
    })
}

/// Name checked against the certificate (`VerifyIdentity`) and sent as SNI.
pub(crate) fn server_name(host: &str) -> Result<ServerName<'static>> {
    // IPv6 literals may come bracketed, as in URLs.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(host.to_owned())
        .map_err(|_| Error::Tls(format!("{host:?} is not a valid host name for TLS")))
}

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|e| {
            Error::Tls(format!(
                "cannot read certificates from {}: {e}",
                path.display()
            ))
        })?;
    if certs.is_empty() {
        return Err(Error::Tls(format!(
            "no PEM certificate found in {}",
            path.display()
        )));
    }
    Ok(certs)
}

fn load_roots(path: &Path) -> Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    for cert in load_certs(path)? {
        roots.add(cert).map_err(|e| {
            Error::Tls(format!("invalid CA certificate in {}: {e}", path.display()))
        })?;
    }
    Ok(roots)
}

fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| {
        Error::Tls(format!(
            "cannot read private key from {}: {e}",
            path.display()
        ))
    })
}

/// `Required` / `Preferred`: encrypt without authenticating the server, but
/// still check that the handshake is signed by the presented certificate.
#[derive(Debug)]
struct NoVerification(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// `VerifyCa`: full chain verification, but any host name is accepted.
#[derive(Debug)]
struct AnyHostName(Arc<dyn ServerCertVerifier>);

impl ServerCertVerifier for AnyHostName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            // The chain was checked before the name, so only the name failed.
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName
                | rustls::CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rcgen::BasicConstraints;
    use rcgen::CertificateParams;
    use rcgen::IsCa;
    use rcgen::Issuer;
    use rcgen::KeyPair;
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::ServerName;
    use rustls::pki_types::UnixTime;

    use super::SslMode;
    use super::TlsOpts;
    use super::client_config;
    use super::server_name;
    use super::verifier;

    /// A CA (written to a temp file) and a server certificate it signed for
    /// `db.example`.
    struct Fixture {
        ca_path: PathBuf,
        server_cert: CertificateDer<'static>,
    }

    fn fixture(tag: &str) -> Fixture {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);

        let server_key = KeyPair::generate().unwrap();
        let server_cert = CertificateParams::new(vec!["db.example".to_owned()])
            .unwrap()
            .signed_by(&server_key, &issuer)
            .unwrap();

        let ca_path =
            std::env::temp_dir().join(format!("rustmy-test-ca-{tag}-{}.pem", std::process::id()));
        std::fs::write(&ca_path, ca_cert.pem()).unwrap();
        Fixture {
            ca_path,
            server_cert: server_cert.der().clone(),
        }
    }

    fn verify(mode: SslMode, ca: &Path, cert: &CertificateDer<'_>, host: &str) -> bool {
        let opts = TlsOpts {
            mode,
            ca: Some(ca.to_path_buf()),
            ..TlsOpts::default()
        };
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        verifier(&opts, &provider)
            .unwrap()
            .verify_server_cert(
                cert,
                &[],
                &ServerName::try_from(host).unwrap(),
                &[],
                UnixTime::now(),
            )
            .is_ok()
    }

    #[test]
    fn verify_identity_checks_chain_and_name() {
        let f = fixture("identity");
        assert!(verify(
            SslMode::VerifyIdentity,
            &f.ca_path,
            &f.server_cert,
            "db.example"
        ));
        assert!(!verify(
            SslMode::VerifyIdentity,
            &f.ca_path,
            &f.server_cert,
            "other.example"
        ));
    }

    #[test]
    fn verify_ca_checks_chain_but_not_name() {
        let f = fixture("ca");
        assert!(verify(
            SslMode::VerifyCa,
            &f.ca_path,
            &f.server_cert,
            "other.example"
        ));
        let stranger = fixture("stranger");
        assert!(!verify(
            SslMode::VerifyCa,
            &stranger.ca_path,
            &f.server_cert,
            "db.example"
        ));
    }

    #[test]
    fn required_accepts_any_certificate() {
        let f = fixture("required");
        let stranger = fixture("required-stranger");
        assert!(verify(
            SslMode::Required,
            &stranger.ca_path,
            &f.server_cert,
            "x.example"
        ));
    }

    #[test]
    fn half_a_client_certificate_is_rejected() {
        let f = fixture("client");
        let opts = TlsOpts {
            mode: SslMode::Required,
            cert: Some(f.ca_path),
            ..TlsOpts::default()
        };
        assert!(client_config(&opts).is_err());
    }

    #[test]
    fn missing_ca_file_is_a_clear_error() {
        let opts = TlsOpts {
            mode: SslMode::VerifyIdentity,
            ca: Some(PathBuf::from("/nonexistent/ca.pem")),
            ..TlsOpts::default()
        };
        let err = client_config(&opts).unwrap_err().to_string();
        assert!(err.contains("/nonexistent/ca.pem"), "{err}");
    }

    #[test]
    fn configurations_are_built_once_per_options_and_files() {
        let f = fixture("cache");
        let opts = TlsOpts {
            mode: SslMode::VerifyIdentity,
            ca: Some(f.ca_path.clone()),
            ..TlsOpts::default()
        };
        let first = client_config(&opts).unwrap();
        assert!(Arc::ptr_eq(&first, &client_config(&opts).unwrap()));
        let other_mode = TlsOpts {
            mode: SslMode::VerifyCa,
            ..opts.clone()
        };
        assert!(!Arc::ptr_eq(&first, &client_config(&other_mode).unwrap()));

        // A renewed CA file gives a new configuration.
        let renewed = fixture("cache-renewed");
        std::fs::copy(&renewed.ca_path, &f.ca_path).unwrap();
        let second = client_config(&opts).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(&second, &client_config(&opts).unwrap()));

        // A file that disappears is reported, not served from the cache.
        std::fs::remove_file(&f.ca_path).unwrap();
        let err = client_config(&opts).unwrap_err().to_string();
        assert!(err.contains(&*f.ca_path.to_string_lossy()), "{err}");
    }

    #[test]
    fn modes_parse_case_insensitively() {
        assert_eq!(
            SslMode::parse("VERIFY_IDENTITY"),
            Some(SslMode::VerifyIdentity)
        );
        assert_eq!(SslMode::parse("required"), Some(SslMode::Required));
        assert_eq!(SslMode::parse("verify-ca"), None);
    }

    #[test]
    fn server_names_accept_dns_and_ip_literals() {
        assert!(server_name("db.example.com").is_ok());
        assert!(server_name("127.0.0.1").is_ok());
        assert!(server_name("[::1]").is_ok());
        assert!(server_name("not a host").is_err());
    }
}
