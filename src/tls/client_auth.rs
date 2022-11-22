
use std::sync::Arc;
use std::io::BufReader;

use rustls::{Certificate, PrivateKey, SignatureScheme};
use rustls::client::ResolvesClientCert;
use rustls::sign::{self, CertifiedKey};
use rustls_pemfile::Item;

use super::{CryptoBytes, CryptoError};

/// A simple implementation of rustls' [`ResolvesClientCert`] that always
/// returns the same certificate and private key pair. Rustls already has
/// one of those, but unfortunately it's not visible to us so we can't use
/// it. This is basically a copy/paste of it.
pub struct ClientCertResolver(Arc<CertifiedKey>);

impl ClientCertResolver
{
    /// Instantiates a new [`ClientCertResolver`] with the specified certificate and
    /// private key. See [`super::super::options::OptionsT::enable_tls_client_auth()`]
    /// for more information.
    pub fn from_bytes(cert_bytes: CryptoBytes, pk_bytes: CryptoBytes) -> Result<ClientCertResolver, CryptoError>
    {
        let cert = match cert_bytes {
            CryptoBytes::Der(bytes) => vec![Certificate(bytes.to_vec())],
            CryptoBytes::Pem(bytes) => {
                let mut rd = BufReader::new(bytes);
                let certs = rustls_pemfile::certs(&mut rd).map_err(CryptoError::IoError)?;

                certs.into_iter().map(Certificate).collect()
            }
        };

        let pk_der = match pk_bytes {
            CryptoBytes::Der(bytes) => bytes.to_vec(),
            CryptoBytes::Pem(bytes) => {
                let mut opt_der = None;
                let mut rd = BufReader::new(bytes);
                
                loop {
                    let der = match rustls_pemfile::read_one(&mut rd) {
                        Ok(Some(Item::PKCS8Key(x))) => Some(x),
                        Ok(Some(Item::RSAKey(x)))   => Some(x),
                        Ok(Some(Item::ECKey(x)))    => Some(x),
                        Ok(Some(_))                 => None,
                        Ok(None)                    => break,
                        Err(err)                    => return Err(CryptoError::IoError(err))
                    };

                    if opt_der.is_none() {
                        opt_der = der;
                    }
                }

                opt_der.ok_or(CryptoError::NoValidItemsInPem)?
            }
        };

        let key = sign::any_supported_type(&PrivateKey(pk_der)).map_err(|_| CryptoError::InvalidPrivateKey)?;
        Ok(ClientCertResolver(Arc::new(CertifiedKey::new(cert, key))))
    }
}

impl ResolvesClientCert for ClientCertResolver
{
    fn has_certs(&self) -> bool { true }

    fn resolve(&self, _acceptable_issuers: &[&[u8]], _sigschemes: &[SignatureScheme]) -> Option<Arc<CertifiedKey>>
    {
        Some(self.0.clone())
    }
}
