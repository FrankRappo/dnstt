use anyhow::Result;
use rcgen::KeyPair;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::path::Path;
use std::sync::Arc;

/// Generate a self-signed TLS certificate for the tunnel.
pub fn generate_self_signed_cert(
    domain: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])
        .map_err(|e| anyhow::anyhow!("cert params error: {}", e))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, domain);

    let key_pair = KeyPair::generate()
        .map_err(|e| anyhow::anyhow!("key generation error: {}", e))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow::anyhow!("self-sign error: {}", e))?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    Ok((vec![cert_der], key_der))
}

/// Save certificate and key to PEM files.
pub fn save_cert_pem(
    cert_path: &Path,
    key_path: &Path,
    domain: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])
        .map_err(|e| anyhow::anyhow!("cert params error: {}", e))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, domain);

    let key_pair = KeyPair::generate()
        .map_err(|e| anyhow::anyhow!("key generation error: {}", e))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow::anyhow!("self-sign error: {}", e))?;

    std::fs::write(cert_path, cert.pem())?;
    std::fs::write(key_path, key_pair.serialize_pem())?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    Ok((vec![cert_der], key_der))
}

/// Load certificate from PEM file.
pub fn load_cert_pem(cert_path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let pem = std::fs::read(cert_path)?;
    let mut certs = Vec::new();
    for cert in rustls_pemfile::certs(&mut &pem[..]) {
        certs.push(cert.map_err(|e| anyhow::anyhow!("pem parse error: {}", e))?);
    }
    if certs.is_empty() {
        anyhow::bail!("no certificates found in {}", cert_path.display());
    }
    Ok(certs)
}

/// Load private key from PEM file.
pub fn load_key_pem(key_path: &Path) -> Result<PrivateKeyDer<'static>> {
    let pem = std::fs::read(key_path)?;
    let key = rustls_pemfile::private_key(&mut &pem[..])
        .map_err(|e| anyhow::anyhow!("key parse error: {}", e))?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path.display()))?;
    Ok(key)
}

/// Build a rustls ServerConfig with self-signed cert.
pub fn server_tls_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<rustls::ServerConfig> {
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow::anyhow!("TLS config error: {}", e))?;
    Ok(config)
}

/// Build a rustls ClientConfig that trusts the given self-signed cert.
pub fn client_tls_config(
    server_certs: Vec<CertificateDer<'static>>,
) -> Result<rustls::ClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    for cert in &server_certs {
        root_store
            .add(cert.clone())
            .map_err(|e| anyhow::anyhow!("add root cert error: {}", e))?;
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    Ok(config)
}

/// Build a rustls ClientConfig that skips certificate verification (for testing).
pub fn client_tls_config_insecure() -> Result<rustls::ClientConfig> {
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();
    Ok(config)
}

#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_self_signed() {
        let (certs, _key) = generate_self_signed_cert("test.example.com").unwrap();
        assert_eq!(certs.len(), 1);
        assert!(!certs[0].is_empty());
    }

    #[test]
    fn test_server_client_tls_configs() {
        let (certs, key) = generate_self_signed_cert("test.example.com").unwrap();
        let _server_config = server_tls_config(certs.clone(), key).unwrap();
        let _client_config = client_tls_config(certs).unwrap();
    }
}
