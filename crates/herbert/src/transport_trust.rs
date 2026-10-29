//! Per-connection server-certificate trust. Normal verification always runs first.
//! The profile option "Allow self-signed certificate" relaxes only an untrusted
//! issuer: the presented chain is re-verified with its own top certificate as the
//! anchor, so the server name, validity dates, key usage and handshake signature
//! are still checked. It never applies globally; each Connection owns its client.
use herbert_contract::Diagnostic;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TrustMode {
    /// Windows trusted the certificate chain.
    Verified,
    /// The issuer was not trusted and the profile allows that; name and dates still passed.
    RelaxedUntrustedIssuer,
    /// Plain http: no TLS, so no certificate was presented or verified.
    Unencrypted,
}

/// What Herbert observed about the server certificate. Public certificate facts only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransportTrust {
    pub mode: TrustMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
}

pub(crate) type Observed = Arc<Mutex<Option<TransportTrust>>>;

/// Only a chain-trust failure is relaxable. Every other certificate error,
/// including revocation, bad signatures and name or date failures, refuses.
pub(crate) fn relaxable(error: &TlsError) -> bool {
    matches!(error, TlsError::InvalidCertificate(CertificateError::UnknownIssuer))
}

#[derive(Debug)]
pub(crate) struct Verifier {
    /// None when the machine trust store is empty: every chain is then untrusted.
    strict: Option<Arc<WebPkiServerVerifier>>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
    allow_untrusted_issuer: bool,
    observed: Observed,
}

pub(crate) fn native_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    // Unreadable individual certificates are skipped, as reqwest's native-roots path does.
    for certificate in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(certificate);
    }
    roots
}

impl Verifier {
    pub(crate) fn new(roots: RootCertStore, allow_untrusted_issuer: bool, observed: Observed) -> Result<Self, Diagnostic> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let strict = if roots.is_empty() { None } else {
            Some(WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().map_err(|_| unavailable())?)
        };
        Ok(Self { strict, algorithms: provider.signature_verification_algorithms, allow_untrusted_issuer, observed })
    }
    fn anchored(anchor: &CertificateDer<'_>) -> Result<Arc<WebPkiServerVerifier>, TlsError> {
        let mut anchors = RootCertStore::empty();
        anchors.add(anchor.clone().into_owned())?;
        WebPkiServerVerifier::builder_with_provider(Arc::new(anchors), Arc::new(rustls::crypto::ring::default_provider()))
            .build().map_err(|_| TlsError::InvalidCertificate(CertificateError::UnknownIssuer))
    }
    fn record(&self, mode: TrustMode, end_entity: &CertificateDer<'_>) {
        let facts = facts(end_entity);
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(TransportTrust {
                mode,
                certificate_sha256: Some(crate::storage::sha256(end_entity.as_ref())),
                subject: facts.as_ref().and_then(|f| f.subject.clone()),
                issuer: facts.as_ref().and_then(|f| f.issuer.clone()),
                not_after: facts.and_then(|f| f.not_after),
            });
        }
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>, ocsp: &[u8], now: UnixTime) -> Result<ServerCertVerified, TlsError> {
        let strict = match &self.strict {
            Some(strict) => strict.verify_server_cert(end_entity, intermediates, server_name, ocsp, now),
            None => Err(TlsError::InvalidCertificate(CertificateError::UnknownIssuer)),
        };
        match strict {
            Ok(verified) => { self.record(TrustMode::Verified, end_entity); Ok(verified) }
            Err(error) if self.allow_untrusted_issuer && relaxable(&error) => {
                // Anchor only on what the server itself presented: each non-leaf
                // certificate from the top of the chain down (a private CA, even when
                // an unrelated extra certificate follows it), then the leaf itself (a
                // self-signed server). webpki still checks the path, name, dates and usage.
                let mut specific = None;
                for anchor in intermediates.iter().rev().chain(std::iter::once(end_entity)) {
                    let Ok(relaxed) = Self::anchored(anchor) else { continue };
                    match relaxed.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
                        Ok(verified) => { self.record(TrustMode::RelaxedUntrustedIssuer, end_entity); return Ok(verified); }
                        // A more specific failure (name, dates) is the real reason to refuse.
                        Err(failure) if !relaxable(&failure) => { specific.get_or_insert(failure); }
                        Err(_) => {}
                    }
                }
                Err(specific.unwrap_or(error))
            }
            Err(error) => Err(error),
        }
    }
    // The server must still prove it holds the certificate's private key.
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// The client TLS configuration for a relaxed profile. A profile without the
/// option keeps reqwest's own native-roots configuration unchanged.
pub(crate) fn relaxed_client_config(observed: Observed) -> Result<rustls::ClientConfig, Diagnostic> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Verifier::new(native_roots(), true, observed)?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions().map_err(|_| unavailable())?
        .dangerous().with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    // Herbert's reqwest build speaks HTTP/1.1 only.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

fn unavailable() -> Diagnostic {
    Diagnostic::new("TRANSPORT_UNAVAILABLE", "Cannot initialize TLS and HTTP transport.")
}

/// A plain, non-retryable diagnostic for a certificate failure, or None when the
/// failure is not about the certificate.
/// Find the rustls certificate error in a transport error chain. hyper wraps it in
/// nested io::Errors, and an io::Error reports its wrapped error's source rather
/// than the wrapped error itself, so unwrap each io::Error explicitly.
fn certificate_error<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a CertificateError> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(TlsError::InvalidCertificate(certificate)) = error.downcast_ref::<TlsError>() {
            return Some(certificate);
        }
        current = match error.downcast_ref::<std::io::Error>().and_then(|io| io.get_ref()) {
            Some(inner) => Some(inner as &(dyn std::error::Error + 'static)),
            None => error.source(),
        };
    }
    None
}

pub(crate) fn certificate_diagnostic(error: &(dyn std::error::Error + 'static), host: &str, relaxed: bool) -> Option<Diagnostic> {
    let mut diagnostic = match certificate_error(error)? {
                CertificateError::UnknownIssuer => Diagnostic::new("CERTIFICATE_UNTRUSTED",
                    "The server's certificate is self-signed or not trusted by Windows. If you trust this server, turn on 'Allow self-signed certificate' for this profile."),
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => Diagnostic::new("CERTIFICATE_NAME_MISMATCH",
                    "The server's certificate was not issued for this server name. Connect using the name on the certificate, or ask the server owner for a certificate that includes this name. 'Allow self-signed certificate' does not change this check."),
                CertificateError::Expired | CertificateError::ExpiredContext { .. } => Diagnostic::new("CERTIFICATE_EXPIRED",
                    "The server's certificate has expired. Ask the server owner to renew it."),
                CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => Diagnostic::new("CERTIFICATE_NOT_YET_VALID",
                    "The server's certificate is not valid yet. Check this computer's date and time, or ask the server owner about the certificate dates."),
                _ => Diagnostic::new("CERTIFICATE_INVALID",
                    "The server's certificate could not be verified. Ask the server owner to check it. 'Allow self-signed certificate' only covers an untrusted issuer."),
            };
    diagnostic.context.insert("host".into(), serde_json::json!(host));
    diagnostic.context.insert("allowUntrustedCertificate".into(), serde_json::json!(relaxed));
    Some(diagnostic)
}

struct Facts { subject: Option<String>, issuer: Option<String>, not_after: Option<String> }

/// Read subject, issuer and notAfter from the DER certificate. Display only;
/// trust decisions never use these strings.
fn facts(certificate: &CertificateDer<'_>) -> Option<Facts> {
    let (_, certificate_body) = der(certificate.as_ref(), 0x30)?;
    let (_, mut tbs) = der(certificate_body, 0x30)?;
    if tbs.first() == Some(&0xa0) { tbs = der(tbs, 0xa0)?.0; } // version
    let tbs = der(tbs, 0x02)?.0; // serial
    let tbs = der(tbs, 0x30)?.0; // signature algorithm
    let (tbs, issuer) = der(tbs, 0x30)?;
    let (tbs, validity) = der(tbs, 0x30)?;
    let (_, subject) = der(tbs, 0x30)?;
    let (after_before, _) = time(validity)?;
    let (_, not_after) = time(after_before)?;
    Some(Facts { subject: name(subject), issuer: name(issuer), not_after: Some(not_after) })
}

/// Returns (rest, content) for one DER element with the expected tag.
fn der(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    if input.first() != Some(&tag) { return None; }
    let (length, header) = match *input.get(1)? {
        short if short < 0x80 => (short as usize, 2),
        long => {
            let count = (long & 0x7f) as usize;
            if count == 0 || count > 4 { return None; }
            let mut length = 0usize;
            for byte in input.get(2..2 + count)? { length = (length << 8) | *byte as usize; }
            (length, 2 + count)
        }
    };
    let end = header.checked_add(length)?;
    Some((input.get(end..)?, input.get(header..end)?))
}

fn time(input: &[u8]) -> Option<(&[u8], String)> {
    let tag = *input.first()?;
    if tag != 0x17 && tag != 0x18 { return None; }
    let (rest, value) = der(input, tag)?;
    let text = std::str::from_utf8(value).ok()?;
    // UTCTime YYMMDDhhmmssZ or GeneralizedTime YYYYMMDDhhmmssZ, shown as ISO 8601.
    let full = if tag == 0x17 {
        let year: u32 = text.get(0..2)?.parse().ok()?;
        format!("{}{}", if year >= 50 { 1900 + year } else { 2000 + year }, text.get(2..)?)
    } else { text.to_owned() };
    let iso = format!("{}-{}-{}T{}:{}:{}Z", full.get(0..4)?, full.get(4..6)?, full.get(6..8)?, full.get(8..10)?, full.get(10..12)?, full.get(12..14)?);
    Some((rest, iso))
}

/// CN, O and OU attributes in the order presented, e.g. "CN=server, O=Example".
fn name(mut input: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    while !input.is_empty() {
        let (rest, set) = der(input, 0x31)?;
        input = rest;
        let (_, attribute) = der(set, 0x30)?;
        let (value_part, oid) = der(attribute, 0x06)?;
        let label = match oid { [0x55, 0x04, 0x03] => "CN", [0x55, 0x04, 0x0a] => "O", [0x55, 0x04, 0x0b] => "OU", _ => continue };
        let tag = *value_part.first()?;
        let (_, value) = der(value_part, tag)?;
        if let Ok(text) = std::str::from_utf8(value) { parts.push(format!("{label}={text}")); }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Test-only certificates (see tls_fixtures/README.md). Their private keys protect nothing.
    use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    pub(crate) fn chain(name: &str) -> Vec<CertificateDer<'static>> {
        let pem = match name {
            "self-signed" => include_str!("tls_fixtures/self-signed.pem"),
            "other-host" => include_str!("tls_fixtures/other-host.pem"),
            "expired" => include_str!("tls_fixtures/expired.pem"),
            "private-ca-leaf" => include_str!("tls_fixtures/private-ca-chain.pem"),
            _ => panic!("unknown fixture {name}"),
        };
        CertificateDer::pem_slice_iter(pem.as_bytes()).collect::<Result<_, _>>().unwrap()
    }
    pub(crate) fn key(name: &str) -> PrivateKeyDer<'static> {
        let pem = match name {
            "self-signed" => include_str!("tls_fixtures/self-signed.key"),
            "other-host" => include_str!("tls_fixtures/other-host.key"),
            "expired" => include_str!("tls_fixtures/expired.key"),
            "private-ca-leaf" => include_str!("tls_fixtures/private-ca-leaf.key"),
            _ => panic!("unknown fixture {name}"),
        };
        PrivateKeyDer::from_pem_slice(pem.as_bytes()).unwrap()
    }
    pub(crate) fn ca_root() -> CertificateDer<'static> {
        CertificateDer::from_pem_slice(include_str!("tls_fixtures/private-ca-root.pem").as_bytes()).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc as StdArc;

    fn now() -> UnixTime { UnixTime::now() }
    fn host() -> ServerName<'static> { ServerName::try_from("localhost").unwrap() }
    fn verifier(allow: bool, roots: RootCertStore) -> (Verifier, Observed) {
        let observed: Observed = Default::default();
        (Verifier::new(roots, allow, observed.clone()).unwrap(), observed)
    }
    fn empty() -> RootCertStore { RootCertStore::empty() }
    fn verify(v: &Verifier, chain: &[CertificateDer<'static>], name: &ServerName<'_>) -> Result<ServerCertVerified, TlsError> {
        v.verify_server_cert(&chain[0], &chain[1..], name, &[], now())
    }

    #[test]
    fn relaxed_option_finds_the_private_ca_when_an_extra_certificate_follows_it() {
        // Leaf, its private CA, then an unrelated certificate the server also sent.
        let mut chain = fixtures::chain("private-ca-leaf");
        assert!(chain.len() >= 2, "fixture is leaf plus private CA");
        chain.push(fixtures::chain("self-signed")[0].clone());
        let (v, observed) = verifier(true, empty());
        assert!(verify(&v, &chain, &host()).is_ok(), "the presented private CA anchors the chain");
        assert_eq!(observed.lock().unwrap().as_ref().unwrap().mode, TrustMode::RelaxedUntrustedIssuer);
        // Still refused without the option, and an unrelated chain still fails.
        let (strict, _) = verifier(false, empty());
        assert!(verify(&strict, &chain, &host()).is_err());
        let unrelated = vec![fixtures::chain("private-ca-leaf")[0].clone(), fixtures::chain("self-signed")[0].clone()];
        let (v, _) = verifier(true, empty());
        assert_eq!(verify(&v, &unrelated, &host()).unwrap_err(), TlsError::InvalidCertificate(CertificateError::UnknownIssuer),
            "a leaf whose issuer was never presented stays untrusted");
        // A wrong name is still the reported reason even when another anchor fails generically.
        let (v, _) = verifier(true, empty());
        let other = ServerName::try_from("other.invalid").unwrap();
        assert!(matches!(verify(&v, &chain, &other).unwrap_err(), TlsError::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. })));
    }

    #[test]
    fn relaxed_option_refuses_chain_errors_other_than_unknown_issuer() {
        assert!(relaxable(&TlsError::InvalidCertificate(CertificateError::UnknownIssuer)));
        for other in [
            CertificateError::Revoked, CertificateError::BadSignature, CertificateError::UnhandledCriticalExtension,
            CertificateError::InvalidPurpose, CertificateError::UnknownRevocationStatus, CertificateError::Expired,
            CertificateError::NotValidForName, CertificateError::BadEncoding,
            CertificateError::Other(rustls::OtherError(StdArc::new(std::io::Error::other("fixture")))),
        ] {
            assert!(!relaxable(&TlsError::InvalidCertificate(other.clone())), "{other:?} must not be relaxed");
        }
        assert!(!relaxable(&TlsError::General("fixture".into())));
    }

    #[test]
    fn self_signed_is_refused_without_the_option_and_accepted_with_it() {
        let chain = fixtures::chain("self-signed");
        let (strict, observed) = verifier(false, empty());
        assert!(matches!(verify(&strict, &chain, &host()), Err(TlsError::InvalidCertificate(CertificateError::UnknownIssuer))));
        assert!(observed.lock().unwrap().is_none());
        let (relaxed, observed) = verifier(true, empty());
        verify(&relaxed, &chain, &host()).unwrap();
        let trust = observed.lock().unwrap().clone().unwrap();
        assert_eq!(trust.mode, TrustMode::RelaxedUntrustedIssuer);
        assert_eq!(trust.certificate_sha256.as_deref(), Some(crate::storage::sha256(chain[0].as_ref()).as_str()));
        assert_eq!(trust.subject.as_deref(), Some("CN=localhost"));
    }

    #[test]
    fn relaxed_option_refuses_certificate_valid_for_a_different_host() {
        let (relaxed, observed) = verifier(true, empty());
        let error = verify(&relaxed, &fixtures::chain("other-host"), &host()).unwrap_err();
        assert!(matches!(error, TlsError::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. })), "{error:?}");
        let error = verify(&relaxed, &fixtures::chain("expired"), &host()).unwrap_err();
        assert!(matches!(error, TlsError::InvalidCertificate(CertificateError::Expired | CertificateError::ExpiredContext { .. })), "{error:?}");
        assert!(observed.lock().unwrap().is_none(), "nothing is recorded for a refused certificate");
    }

    #[test]
    fn trusted_certificate_reports_verified_even_with_option_on() {
        let chain = fixtures::chain("private-ca-leaf");
        let (relaxed, observed) = verifier(true, empty());
        verify(&relaxed, &chain, &host()).unwrap();
        assert_eq!(observed.lock().unwrap().as_ref().unwrap().mode, TrustMode::RelaxedUntrustedIssuer);
        // The same chain under an injected trusted root verifies normally, option on or off.
        for allow in [false, true] {
            let mut roots = empty();
            roots.add(fixtures::ca_root()).unwrap();
            let (v, observed) = verifier(allow, roots);
            verify(&v, &chain, &host()).unwrap();
            assert_eq!(observed.lock().unwrap().as_ref().unwrap().mode, TrustMode::Verified);
        }
    }

    #[test]
    fn certificate_failures_have_plain_codes_and_point_to_the_option() {
        let cases = [
            (CertificateError::UnknownIssuer, "CERTIFICATE_UNTRUSTED"),
            (CertificateError::NotValidForName, "CERTIFICATE_NAME_MISMATCH"),
            (CertificateError::Expired, "CERTIFICATE_EXPIRED"),
            (CertificateError::NotValidYet, "CERTIFICATE_NOT_YET_VALID"),
            (CertificateError::Revoked, "CERTIFICATE_INVALID"),
        ];
        for (error, code) in cases {
            let wrapped = std::io::Error::other(TlsError::InvalidCertificate(error));
            let diagnostic = certificate_diagnostic(&wrapped, "erp.example.test", false).unwrap();
            assert_eq!(diagnostic.code, code);
        }
        let untrusted = certificate_diagnostic(&std::io::Error::other(TlsError::InvalidCertificate(CertificateError::UnknownIssuer)), "h", false).unwrap();
        assert_eq!(untrusted.message, "The server's certificate is self-signed or not trusted by Windows. If you trust this server, turn on 'Allow self-signed certificate' for this profile.");
        assert!(certificate_diagnostic(&std::io::Error::other("connection reset"), "h", false).is_none());
        // hyper's actual shape: io::Error(Other) wrapping io::Error(InvalidData) wrapping rustls.
        let nested = std::io::Error::other(std::io::Error::new(std::io::ErrorKind::InvalidData, TlsError::InvalidCertificate(CertificateError::UnknownIssuer)));
        assert_eq!(certificate_diagnostic(&nested, "h", false).unwrap().code, "CERTIFICATE_UNTRUSTED");
    }

    // ---- Real TLS handshakes against a loopback server (test certificates only).
    use crate::connection::{Connection, ConnectionOptions};
    use crate::input::{AuthMode, Credentials};
    use crate::test_http::Server;
    use serde_json::json;
    fn epicor() -> crate::test_http::Router {
        StdArc::new(|method: &str, _path: &str, _body: &serde_json::Value| match method {
            "GetUserFile" => (200, json!({"returnObj":{"UserFile":[{"DcdUserID":"manager","UserID":"manager","SecurityMgr":true,"UserDisabled":false,"LockedOut":false,"CompList":"EPIC"}],
                "UserComp":[{"Company":"EPIC","CompanyName":"Epic"}]}})),
            "GetDatabaseVersion" => (200, json!({"returnObj":"5.2.100"})),
            _ => (200, json!({})),
        })
    }
    fn connection(server: &Server, allow: bool) -> StdArc<Connection> {
        Connection::with_options(server.endpoint.clone(), Some("EPIC".into()), AuthMode::BasicApiKey,
            Credentials { username: "manager".into(), password: "fixture".into(), api_key: Some("fixture".into()) },
            Default::default(), crate::scheduler::Budget::new(2).unwrap(), ConnectionOptions { allow_untrusted_certificate: allow }).unwrap()
    }
    async fn connect(server: &Server, allow: bool) -> (Result<serde_json::Value, Diagnostic>, StdArc<Connection>) {
        let connection = connection(server, allow);
        (connection.connect(&tokio_util::sync::CancellationToken::new()).await, connection)
    }

    #[tokio::test]
    async fn self_signed_refused_when_option_off_with_actionable_message() {
        let server = Server::start(Some("self-signed"), epicor());
        let (result, _) = connect(&server, false).await;
        let error = result.unwrap_err();
        assert_eq!(error.code, "CERTIFICATE_UNTRUSTED", "{error:?}");
        assert_eq!(error.message, "The server's certificate is self-signed or not trusted by Windows. If you trust this server, turn on 'Allow self-signed certificate' for this profile.");
        assert!(!error.retryable);
        assert!(server.calls.lock().unwrap().is_empty(), "no HTTP request crosses a refused handshake");
    }

    #[tokio::test]
    async fn self_signed_accepted_when_option_on_for_that_connection() {
        let server = Server::start(Some("self-signed"), epicor());
        let (result, connection) = connect(&server, true).await;
        result.unwrap();
        let trust = connection.transport_trust();
        assert_eq!(trust.mode, TrustMode::RelaxedUntrustedIssuer);
        assert_eq!(trust.certificate_sha256.as_deref(), Some(crate::storage::sha256(fixtures::chain("self-signed")[0].as_ref()).as_str()));
        assert_eq!(trust.not_after.as_deref(), Some("2126-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn relaxed_option_does_not_affect_other_profiles_or_connections() {
        let server = Server::start(Some("self-signed"), epicor());
        let (relaxed, _) = connect(&server, true).await;
        relaxed.unwrap();
        let (strict, _) = connect(&server, false).await;
        assert_eq!(strict.unwrap_err().code, "CERTIFICATE_UNTRUSTED", "a second connection to the same server keeps its own setting");
        let (relaxed_again, _) = connect(&server, true).await;
        relaxed_again.unwrap();
    }

    #[tokio::test]
    async fn relaxed_option_still_refuses_name_mismatch() {
        let server = Server::start(Some("other-host"), epicor());
        let (result, _) = connect(&server, true).await;
        let error = result.unwrap_err();
        assert_eq!(error.code, "CERTIFICATE_NAME_MISMATCH", "{error:?}");
        assert!(error.message.contains("not issued for this server name"), "{}", error.message);
        assert_eq!(error.context["host"], "localhost");
        assert!(server.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn relaxed_option_still_refuses_expired_certificate() {
        let server = Server::start(Some("expired"), epicor());
        let (result, _) = connect(&server, true).await;
        assert_eq!(result.unwrap_err().code, "CERTIFICATE_EXPIRED");
        assert!(server.calls.lock().unwrap().is_empty());
    }
}
