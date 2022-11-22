use std::time::SystemTime;
use rustls::client::{ServerCertVerifier, ServerCertVerified};
use rustls::{Certificate, ServerName, Error};
use log::warn;

/// An implementation of rustls' [`ServerCertVerifier`] that simply skips
/// the server's certificate authenticity check. This is a **very dangerous**
/// operation and is not recommended as it renders the connection **INSECURE**.
/// 
/// See [`super::super::options::OptionsT::enable_dangerous_non_verified_tls()`]
pub struct SkipServerCertVerification;

impl ServerCertVerifier for SkipServerCertVerification
{
    fn verify_server_cert(&self, _end_entity: &Certificate, _intermediates: &[Certificate], server_name: &ServerName, _scts: &mut dyn Iterator<Item = &[u8]>, _ocsp_response: &[u8], _now: SystemTime) -> Result<ServerCertVerified, Error>
    {
        warn!("Skipping server certificate verification for {:?}; THE CONNECTION IS NOT SECURE!!", server_name);
        Ok(ServerCertVerified::assertion())
    }
}
