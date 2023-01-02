use std::io::{self, Read, Write};
use std::ops::DerefMut;
use std::sync::Arc;

use tokio::net::TcpStream;
use rustls::{ClientConfig, ConnectionCommon};
use webpki::Error as BadCertError;

#[cfg(feature = "dangerous_tls")]
pub(super) mod dangerous;
mod client_auth;

pub use client_auth::ClientCertResolver;
use crate::options::OptionsT;
use crate::transport::{ReadyFor, Transport as TransportTrait, WantedFlags};

/// An enum used to specify the format of raw bytes of a TLS certificate
/// or private key.
pub enum CryptoBytes<'a>
{
    /// Use this if the underlying bytes use the DER format
    Der(&'a [u8]),

    /// Use this if the underlying bytes use the PEM format
    Pem(&'a [u8])
}

/// An enum wrapping all the errors that could happen during TLS
/// certificate or private key loading.
#[derive(Debug)]
pub enum CryptoError
{
    /// The certificate is invalid
    BadCert(BadCertError),

    /// An IO error occurred while trying to process the input
    IoError(io::Error),

    /// The PEM bytes did not contain any relevant data. This can happen if,
    /// for instance, you pass a private key while the system expects a
    /// certificate.
    NoValidItemsInPem,

    /// The specified key cannot be used for signing. I'm as clueless as you
    /// are. This indicate that rustls returned a [`rustls::sign::SignError`],
    /// but this error itself lacks information.
    InvalidPrivateKey
}

pub type OptionsWithTls<'a> = OptionsT<'a, ClientConfig>;

impl<'a> OptionsWithTls<'a>
{
    /// Enables TLS client authentication using the specified raw certificate
    /// and private key bytes.
    /// 
    /// # Parameters
    /// 
    /// - `cert_bytes` should contain a reference to the raw bytes of
    ///    your certificate either in DER or PEM format. See [`CryptoBytes`]
    ///    for more information.
    /// - `pk_bytes` should contain a reference to the raw bytes of
    ///    your private key either in DER or PEM format. See [`CryptoBytes`]
    ///    for more information. RSA, EC, and PKCS8 *should* be supported,
    ///    however I only tested with a RSA key.
    /// 
    /// # Return value
    /// 
    /// Because the bytes specified in `cert_bytes` and `pk_bytes` can be
    /// invalid, this function can fail. See [`CryptoError`] for more
    /// information.
    /// 
    /// # Example
    /// 
    /// ```
    /// use std::fs;
    /// use lmc::Options;
    /// use lmc::tls::CryptoBytes;
    /// 
    /// let cert_bytes = fs::read("test_data/client.pem").expect("Failed to load certificate bytes");
    /// let pk_bytes = fs::read("test_data/client_private.pem").expect("Failed to load private key bytes");
    /// 
    /// let mut opts = Options::new("client_id")
    ///     .enable_tls()
    ///     .expect("Failed to load the system's TLS certificates");
    /// 
    /// opts.enable_tls_client_auth(CryptoBytes::Pem(&cert_bytes), CryptoBytes::Pem(&pk_bytes));
    /// ```
    pub fn enable_tls_client_auth(&mut self, cert_bytes: CryptoBytes, pk_bytes: CryptoBytes) -> Result<&mut Self, CryptoError>
    {
        self.connection_cfg.client_auth_cert_resolver = Arc::new(ClientCertResolver::from_bytes(cert_bytes, pk_bytes)?);
        Ok(self)
    }
}

/// A simple struct that implements [`Read`] and [`Write`] by
/// calling [`TcpStream::try_read()`] and [`TcpStream::try_write()`],
/// respectively. This is need to make rustls read and write from/to
/// a tokio TCP stream.
struct TryRWProxy<'a>(&'a TcpStream);

impl<'a> Read for TryRWProxy<'a>
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>
    {
        self.0.try_read(buf)
    }
}

impl<'a> Write for TryRWProxy<'a>
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.try_write(buf)
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

/// A [`TransportTrait`] that implements TLS over TCP.
pub(super) struct Transport<T>
{
    tcp: TcpStream,
    tls: T
}

impl<T> Transport<T>
{
    pub(super) fn new(tcp: TcpStream, tls: T) -> Self
    {
        Self { tcp, tls }
    }
}

impl<D, T: DerefMut<Target = ConnectionCommon<D>> + Send> TransportTrait for Transport<T>
{
    fn wants(&self, _rd_hint: bool, _wr_hint: bool) -> WantedFlags
    {
        WantedFlags { read: self.tls.wants_read(), write: self.tls.wants_write() }
    }

    fn ready_for(&self) -> ReadyFor
    {
        ReadyFor::wrap(&self.tcp)
    }

    fn pre_read(&mut self) -> io::Result<bool>
    {
        if let Err(err) = self.tls.read_tls(&mut TryRWProxy(&self.tcp)) {
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(false);
            }
        }

        let tls_state = self.tls.process_new_packets().map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
        Ok(tls_state.plaintext_bytes_to_read() > 0)
    }

    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize>
    {
        self.tls.reader().read(dst)
    }

    fn pre_write(&mut self) -> io::Result<()>
    {
        match self.tls.write_tls(&mut TryRWProxy(&self.tcp)) {
            Ok(written) => if written <= 0 {
                Err(io::ErrorKind::UnexpectedEof.into())
            } else {
                Ok(())
            },
            Err(err) => if err.kind() == io::ErrorKind::WouldBlock { Ok(()) } else { Err(err) }
        }
    }

    fn write(&mut self, src: &[u8], zero_if_would_block: bool) -> io::Result<usize>
    {
        let written = self.tls.writer().write(src)?;

        if written <= 0 && !zero_if_would_block {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(written)
        }
    }

    fn flush(&mut self) -> io::Result<()>
    {
        self.tls.writer().flush()
    }

    fn send_close_notify(&mut self)
    {
        self.tls.send_close_notify()
    }
}
