//! How the native client decides whether to trust a host's QUIC certificate.
//!
//! The client sends a bearer login token over the connection, so it must know
//! who it is talking to: a host that hands out a certificate hash is pinned to
//! it (the same hash the browser pins for WebTransport); a host named by a
//! public address must hold a certificate the public web PKI vouches for; only
//! a loopback address (a local single-player server, a dev server with token
//! auth) skips verification. [`policy_for`] makes that choice and nothing else
//! can reach [`VerifyPolicy::Insecure`].
//!
//! Pure rustls and `sha2`: no Bevy, no window, native only (the browser does not
//! use QUIC).

use std::{net::IpAddr, sync::Arc};

use quinn::rustls::{
    self,
    client::{
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        WebPkiServerVerifier,
    },
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use sha2::{Digest, Sha256};

/// Whom the client trusts on one QUIC connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyPolicy {
    /// Exactly the leaf certificate whose SHA-256 (hex, any case) this is.
    Pin(String),
    /// A certificate the public web PKI (`webpki-roots`) vouches for, for the
    /// target's host name.
    WebPki,
    /// Anything. Loopback development only.
    Insecure,
}

/// Pin when the endpoint carries a hash; otherwise [`VerifyPolicy::Insecure`] for a
/// loopback address or `localhost` (dev, token auth); otherwise
/// [`VerifyPolicy::WebPki`]. `target` is `host:port` as the endpoint names it.
pub fn policy_for(target: &str, cert_hash: Option<&str>) -> VerifyPolicy {
    if let Some(hash) = cert_hash.map(str::trim).filter(|h| !h.is_empty()) {
        return VerifyPolicy::Pin(hash.to_string());
    }
    if is_loopback(host_of(target)) {
        VerifyPolicy::Insecure
    } else {
        VerifyPolicy::WebPki
    }
}

/// The host part of `host:port`: a name, an IPv4 address, or an IPv6 address
/// (brackets removed). A bare IPv6 address without a port is returned whole.
pub fn host_of(target: &str) -> &str {
    let target = target.trim();
    if let Some(rest) = target.strip_prefix('[') {
        return rest.split_once(']').map_or(rest, |(host, _)| host);
    }
    match target.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => target,
    }
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The TLS client configuration for `policy`. The `ring` provider is named, never
/// left to rustls's process default (the workspace may enable another).
pub fn client_crypto(policy: &VerifyPolicy) -> rustls::ClientConfig {
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions");
    let verifier: Arc<dyn ServerCertVerifier> = match policy {
        VerifyPolicy::Pin(hash) => Arc::new(PinVerifier { hash: hash.clone(), provider: provider() }),
        VerifyPolicy::WebPki => web_pki_verifier(),
        VerifyPolicy::Insecure => Arc::new(SkipServerVerification { provider: provider() }),
    };
    builder.dangerous().with_custom_certificate_verifier(verifier).with_no_client_auth()
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn web_pki_verifier() -> Arc<dyn ServerCertVerifier> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .expect("the bundled web roots are not empty")
}

/// Lower-case hex SHA-256 of a certificate's DER.
fn leaf_hash(cert: &CertificateDer<'_>) -> String {
    use std::fmt::Write;
    Sha256::digest(cert.as_ref()).iter().fold(String::with_capacity(64), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// The host's certificate is not the one its endpoint pinned. `Debug` is the sentence too:
/// rustls prints a [`rustls::CertificateError::Other`] through it.
struct NotPinned;

impl std::fmt::Display for NotPinned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the host's certificate is not the pinned one")
    }
}

impl std::fmt::Debug for NotPinned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for NotPinned {}

/// Whether a connect ended because this client refused the host's certificate: the local
/// connection error's code is a TLS alert about a certificate (a wrong pin, an expired,
/// unissued or self-signed certificate on a public name). Retrying cannot help — the host
/// presents the same certificate — so the connect fails at once with the reason.
pub fn refused_certificate(code: quinn::TransportErrorCode) -> bool {
    use rustls::AlertDescription as A;
    let code = u64::from(code);
    // QUIC carries a TLS alert as the transport error 0x100 + alert (RFC 9001 §4.8).
    if code & !0xff != 0x100 {
        return false;
    }
    matches!(
        A::from((code & 0xff) as u8),
        A::BadCertificate
            | A::UnsupportedCertificate
            | A::CertificateRevoked
            | A::CertificateExpired
            | A::CertificateUnknown
            | A::UnknownCA
            | A::AccessDenied
    )
}

/// Accepts the one leaf whose SHA-256 is `hash`; names and dates are not
/// checked (as the service links), the handshake signatures are.
#[derive(Debug)]
struct PinVerifier {
    hash: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if leaf_hash(end_entity).eq_ignore_ascii_case(&self.hash) {
            Ok(ServerCertVerified::assertion())
        } else {
            // A certificate error (alert certificate_unknown), not `General` (handshake_failure):
            // [`refused_certificate`] tells it from other handshake failures and the connect
            // fails at once instead of retrying.
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::Other(rustls::OtherError(Arc::new(
                NotPinned,
            )))))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Treats any certificate as valid: vulnerable to MITM, loopback development
/// only. Private: [`policy_for`] is the only way to ask for it.
#[derive(Debug)]
struct SkipServerVerification {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
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
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(name: &str) -> CertificateDer<'static> {
        rcgen::generate_simple_self_signed(vec![name.into()]).unwrap().cert.der().clone()
    }

    fn verify(verifier: &dyn ServerCertVerifier, cert: &CertificateDer<'_>) -> Result<ServerCertVerified, rustls::Error> {
        verifier.verify_server_cert(cert, &[], &ServerName::try_from("localhost").unwrap(), &[], UnixTime::now())
    }

    fn pin(hash: String) -> PinVerifier {
        PinVerifier { hash, provider: provider() }
    }

    #[test]
    fn a_hash_always_pins() {
        assert_eq!(policy_for("127.0.0.1:6466", Some("ab")), VerifyPolicy::Pin("ab".into()));
        assert_eq!(policy_for("game.example.org:1", Some("ab")), VerifyPolicy::Pin("ab".into()));
        // An empty hash is no hash.
        assert_eq!(policy_for("game.example.org:1", Some(" ")), VerifyPolicy::WebPki);
    }

    #[test]
    fn loopback_without_a_hash_is_dev() {
        for t in ["127.0.0.1:6466", "[::1]:6466", "localhost:6466"] {
            assert_eq!(policy_for(t, None), VerifyPolicy::Insecure, "{t}");
        }
    }

    #[test]
    fn a_public_name_without_a_pin_uses_web_pki() {
        assert_eq!(policy_for("game.errantry.renownandruin.com:30466", None), VerifyPolicy::WebPki);
        assert_eq!(policy_for("5.9.7.138:30466", None), VerifyPolicy::WebPki);
        assert_eq!(policy_for("localhost.example.com:1", None), VerifyPolicy::WebPki);
    }

    #[test]
    fn the_host_part_drops_the_port_and_brackets() {
        assert_eq!(host_of("game.example.org:30466"), "game.example.org");
        assert_eq!(host_of("[::1]:6466"), "::1");
        assert_eq!(host_of("127.0.0.1"), "127.0.0.1");
        assert_eq!(host_of("::1"), "::1");
    }

    #[test]
    fn a_wrong_pin_is_refused() {
        let (a, b) = (cert("localhost"), cert("localhost"));
        assert!(verify(&pin(leaf_hash(&b)), &a).is_err());
    }

    #[test]
    fn the_right_pin_is_accepted_whatever_its_name_and_case() {
        let a = cert("some.other.name");
        verify(&pin(leaf_hash(&a).to_uppercase()), &a).expect("the pinned leaf is accepted");
        verify(&pin(leaf_hash(&a)), &a).expect("lower case too");
    }

    #[test]
    fn web_pki_refuses_a_self_signed_cert() {
        assert!(verify(web_pki_verifier().as_ref(), &cert("localhost")).is_err());
    }

    #[test]
    fn only_a_certificate_alert_is_a_refused_certificate() {
        use quinn::TransportErrorCode as C;
        use rustls::AlertDescription as A;
        let alert = |a: A| C::crypto(u8::from(a));
        for refused in [A::BadCertificate, A::CertificateExpired, A::CertificateUnknown, A::UnknownCA, A::AccessDenied] {
            assert!(refused_certificate(alert(refused)), "{refused:?}");
        }
        for other in [A::HandshakeFailure, A::NoApplicationProtocol, A::InternalError] {
            assert!(!refused_certificate(alert(other)), "{other:?}");
        }
        assert!(!refused_certificate(C::PROTOCOL_VIOLATION));
        assert!(!refused_certificate(C::CONNECTION_REFUSED));
    }

    #[test]
    fn a_wrong_pin_is_a_certificate_error() {
        let (a, b) = (cert("localhost"), cert("localhost"));
        let error = verify(&pin(leaf_hash(&b)), &a).expect_err("refused");
        assert!(matches!(error, rustls::Error::InvalidCertificate(_)), "{error:?}");
        assert!(error.to_string().contains("not the pinned one"), "{error}");
    }

    #[test]
    fn every_policy_builds_a_config() {
        for policy in [VerifyPolicy::Pin("ab".into()), VerifyPolicy::WebPki, VerifyPolicy::Insecure] {
            client_crypto(&policy);
        }
    }
}
