use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpStream;

use super::QoS;
use super::transport::{Transport, TcpTransport};
use super::errors::ConnectError;

/// [`LastWill`] describes a message to be published if the client disconnects unexpectedly,
/// e.g. if the socket gets closed before a `DISCONNECT` packet could be sent by the client.
pub struct LastWill<'a>
{
    /// The topic to publish in
    pub topic: &'a str,

    /// The message (aka payload) to publish
    pub message: &'a [u8],

    /// Whether the last will message should be retained
    pub retain: bool,

    /// The quality of service of the last will message
    pub qos: QoS
}

/// A bitfield used to represent what versions of the Internet Protocol (IP) to enable.
/// 
/// The set is initially constructed with all versions enabled by default (IPv4 and IPV6)
/// using [`IpVersionSet::all()`].
#[derive(Debug, Clone, Copy)]
pub struct IpVersionSet(u8);

impl IpVersionSet
{
    const V4_BIT: u8 = 1;
    const V6_BIT: u8 = 2;

    /// Constructs an [`IpVersionSet`] with both IPv4 and IPv6 enabled.
    pub fn all() -> Self
    {
        Self(Self::V4_BIT | Self::V6_BIT)
    }

    /// Creates a copy of this set that excludes IPv4.
    pub fn without_v4(self) -> Self
    {
        Self(self.0 & !Self::V4_BIT)
    }

    /// Creates a copy of this set that excludes IPv6.
    pub fn without_v6(self) -> Self
    {
        Self(self.0 & !Self::V6_BIT)
    }

    /// Checks whether the IP version associated with the specified [`SocketAddr`] is
    /// included in this set.
    pub fn supports(self, addr: &SocketAddr) -> bool
    {
        match addr {
            SocketAddr::V4(_) => (self.0 & Self::V4_BIT) != 0,
            SocketAddr::V6(_) => (self.0 & Self::V6_BIT) != 0
        }
    }
}

/// A trait to wrap the creation of [`Transport`]. Mainly used for TLS purposes.
pub trait ConnectionConfig<T>
{
    /// Creates a "connection" tied to the specified hostname. Does nothing
    /// for non-TLS connections.
    fn create_connection(self, host: &str) -> Result<T, ConnectError>;

    /// Creates the transport based on a [`TcpStream`] and the previously-created
    /// connection.
    fn create_transport(stream: TcpStream, connection: T) -> Box<dyn Transport>;
}

impl ConnectionConfig<()> for ()
{
    fn create_connection(self, _host: &str) -> Result<(), ConnectError>
    {
        Ok(())
    }

    fn create_transport(stream: TcpStream, _connection: ()) -> Box<dyn Transport>
    {
        Box::new(TcpTransport::new(stream))
    }
}

/// A struct containing all the MQTT protocol & implementation settings. To create this
/// structure with its default values, use [`Options::new`].
/// 
/// # Example
/// 
/// ```
/// use lmc::{Options, QoS};
/// 
/// let mut opts = Options::new("client_id")
///     .enable_tls()
///     .expect("Failed to load native system TLS certificates");
///     
/// opts.set_last_will("status", b"unexpected_disconnect", true, QoS::AtLeastOnce)
///     .set_keep_alive(10)
///     .set_clean_session()
///     .set_no_delay();
/// ```
pub struct OptionsT<'a, T>
{
    /// Whether to establish a persistent session or not. If this is set to `true`,
    /// then the broker will drop any pre-existing session information associated
    /// with the specified [`Self::client_id`]. This means that when subscribing to
    /// topics, any retained messages will be (re-)transmitted to this client. In
    /// addition, the broker will not store any information about this session.
    /// 
    /// Default is `false`.
    pub clean_session: bool,

    /// Time interval (in seconds) between any packet and a `PING` requests.
    /// 
    /// Default is 30 seconds.
    pub keep_alive: u16,

    /// A string identifying this client. The broker may choose to use soemthing
    /// different.
    /// 
    /// It is also used by the broker to establish a persistent session (should
    /// [`Self::clean_session`] be `false`).
    pub client_id: &'a str,

    /// The [`LastWill`] is used to publish a message if the client connection ends
    /// in an unexpected manner. This is optional.
    /// 
    /// Default is `None`.
    pub last_will: Option<LastWill<'a>>,

    /// An optional username used to authenticate the client to the broker.
    /// 
    /// Default is `None`.
    pub username: Option<&'a str>,

    /// An optional password used to authenticate the client to the broker.
    /// 
    /// Default is `None`.
    pub password: Option<&'a [u8]>,

    /// Sets the "no delay" flag to the TCP connection.
    /// 
    /// Default is `false`.
    /// 
    /// See [`std::net::TcpStream::set_nodelay()`]
    pub no_delay: bool,

    /// A set specifying which Internet Protocol versions to use (IPv4 and/or
    /// IPv6) to establish the TCP connection.
    /// 
    /// By default, both IP versions are enabled.
    pub enabled_ip_versions: IpVersionSet,

    /// The TLS configuration used to establish the connection. By default,
    /// this field is empty. Enabling TLS can be done by first enabling the
    /// `tls` feature in the crate and then using one of the dedicated
    /// functions of [`OptionsT`], such as [`OptionsT::enable_tls()`].
    pub connection_cfg: T,

    /// The maximum amount of time that can be spent looking up the broker's
    /// hostname.
    /// 
    /// By default, this is set to 3 seconds.
    pub dns_timeout: Duration,

    /// The maximum amount of time that can be spent establishing the TCP
    /// connection.
    /// 
    /// By default, this is set to 10 seconds.
    pub tcp_connect_timeout: Duration,

    /// The maximum amount of time waiting for the broker's `CONNACK` packet.
    /// 
    /// By default, this is set to 3 seconds.
    pub mqtt_connect_timeout: Duration,

    /// How long the client should wait before re-sending a packet if no
    /// acknowledment packet is received. This delay has a low precision.
    /// 
    /// Default (and minimum) is 1 second.
    pub packets_resend_delay: Duration,

    /// Default port to use if none is specified in the hostname string.
    /// This field is not accessible. Use [`OptionsT::set_default_port()`]
    /// to change it and [`OptionsT::default_port()`] to access it.
    /// 
    /// Default is 1883 when TLS is **not** used and 8883 when TLS is **in use**.
    default_port: u16,

    /// True if [`Self::default_port`] has been changed through a call to
    /// [`OptionsT::set_default_port()`]. This is used to switch the default
    /// port to 8883 when enabling TLS, if the developer didn't change it
    /// beforehand.
    default_port_changed: bool
}

/// A flavour of [`OptionsT`] with TLS disabled.
pub type Options<'a> = OptionsT<'a, ()>;

impl<'a> Options<'a>
{
    /// Creates new options with the default values and TLS disabled. TLS
    /// can be enabled later on using one of the functions of [`OptionsT`]
    /// such as [`OptionsT::enable_tls()`] (available only if the `tls`
    /// feature of the crate is enabled).
    pub fn new(client_id: &'a str) -> Self
    {
        Self {
            clean_session: false,
            keep_alive: 30,
            client_id: client_id.into(),
            last_will: None,
            username: None,
            password: None,
            no_delay: false,
            enabled_ip_versions: IpVersionSet::all(),
            connection_cfg: (),
            dns_timeout: Duration::from_secs(3),
            tcp_connect_timeout: Duration::from_secs(10),
            mqtt_connect_timeout: Duration::from_secs(3),
            packets_resend_delay: Duration::from_secs(1),
            default_port: 1883,
            default_port_changed: false
        }
    }
}

impl<'a, T> OptionsT<'a, T>
{
    /// Changes the type of the [`Self::tls_config`] field.
    pub(super) fn map_connection_cfg<O, F>(self, f: F) -> OptionsT<'a, O>
    where F: FnOnce(T) -> O
    {
        //Need `type_changing_struct_update` to be stable before we can use `..self`

        OptionsT {
            clean_session: self.clean_session,
            keep_alive: self.keep_alive,
            client_id: self.client_id,
            last_will: self.last_will,
            username: self.username,
            password: self.password,
            no_delay: self.no_delay,
            enabled_ip_versions: self.enabled_ip_versions,
            connection_cfg: f(self.connection_cfg),
            dns_timeout: self.dns_timeout,
            tcp_connect_timeout: self.tcp_connect_timeout,
            mqtt_connect_timeout: self.mqtt_connect_timeout,
            packets_resend_delay: self.packets_resend_delay,
            default_port: self.default_port,
            default_port_changed: self.default_port_changed
        }
    }

    /// Sets the [`Self::clean_session`] flag to `true`. See the corresponding
    /// field's documentation for more information.
    pub fn set_clean_session(&mut self) -> &mut Self
    {
        self.clean_session = true;
        self
    }

    /// Changes [`Self::keep_alive`] to the specified value, in seconds. See
    /// the corresponding field's documentation for more information.
    pub fn set_keep_alive(&mut self, keep_alive: u16) -> &mut Self
    {
        self.keep_alive = keep_alive;
        self
    }

    /// Enables MQTT's last will functionality with the specified settings.
    /// See [`Self::last_will`] for more information.
    pub fn set_last_will_ex(&mut self, last_will: LastWill<'a>) -> &mut Self
    {
        self.last_will = Some(last_will);
        self
    }

    /// Enables MQTT's last will functionality with the specified settings.
    /// See [`Self::last_will`] for more information.
    pub fn set_last_will(&mut self, topic: &'a str, message: &'a [u8], retain: bool, qos: QoS) -> &mut Self
    {
        self.set_last_will_ex(LastWill { topic, message, retain, qos })
    }

    /// Changes [`Self::username`] to the specified value. See the
    /// corresponding field's documentation for more information.
    pub fn set_username(&mut self, username: &'a str) -> &mut Self
    {
        self.username = Some(username);
        self
    }

    /// Changes [`Self::password`] to the specified value. See the
    /// corresponding field's documentation for more information.
    pub fn set_password(&mut self, password: &'a [u8]) -> &mut Self
    {
        self.password = Some(password);
        self
    }

    /// Sets [`Self::no_delay`] to `true`. See the corresponding field's
    /// documentation for more information.
    pub fn set_no_delay(&mut self) -> &mut Self
    {
        self.no_delay = true;
        self
    }

    /// Removes IPv4 from the [`Self::enabled_ip_versions`] set. See the
    /// corresponding field's documentation for more information.
    pub fn disable_ipv4(&mut self) -> &mut Self
    {
        self.enabled_ip_versions = self.enabled_ip_versions.without_v4();
        self
    }

    /// Removes IPv6 from the [`Self::enabled_ip_versions`] set. See the
    /// corresponding field's documentation for more information.
    pub fn disable_ipv6(&mut self) -> &mut Self
    {
        self.enabled_ip_versions = self.enabled_ip_versions.without_v6();
        self
    }

    /// Changes [`Self::dns_timeout`] to the specified value. See the
    /// corresponding field's documentation for more information.
    pub fn set_dns_timeout(&mut self, to: Duration) -> &mut Self
    {
        self.dns_timeout = to;
        self
    }

    /// Changes [`Self::tcp_connect_timeout`] to the specified value. See
    /// the corresponding field's documentation for more information.
    pub fn set_tcp_connect_timeout(&mut self, to: Duration) -> &mut Self
    {
        self.tcp_connect_timeout = to;
        self
    }

    /// Changes [`Self::mqtt_connect_timeout`] to the specified value. See
    /// the corresponding field's documentation for more information.
    pub fn set_mqtt_connect_timeout(&mut self, to: Duration) -> &mut Self
    {
        self.mqtt_connect_timeout = to;
        self
    }

    /// Changes [`Self::dns_timeout`], [`Self::tcp_connect_timeout`], and
    /// [`Self::mqtt_connect_timeout`] to the specified values. See their
    /// respective documentation for more information.
    pub fn set_all_timeouts(&mut self, dns: Duration, tcp_connect: Duration, mqtt_connect: Duration) -> &mut Self
    {
        self.dns_timeout = dns;
        self.tcp_connect_timeout = tcp_connect;
        self.mqtt_connect_timeout = mqtt_connect;

        self
    }

    /// Sets [`Self::dns_timeout`], [`Self::tcp_connect_timeout`], and
    /// [`Self::mqtt_connect_timeout`] to the same specified value. See their
    /// respective documentation for more information.
    pub fn set_all_timeouts_to(&mut self, to: Duration) -> &mut Self
    {
        self.dns_timeout = to;
        self.tcp_connect_timeout = to;
        self.mqtt_connect_timeout = to;

        self
    }

    /// Changes [`Self::packets_resend_delay`] to the specified value. See
    /// the corresponding field's documentation for more information.
    pub fn set_packets_resend_delay(&mut self, delay: Duration) -> &mut Self
    {
        self.packets_resend_delay = delay;
        self
    }

    /// Changes the default port (the port used if no port is explicitly
    /// specified in the hostname passed to [`super::Client::connect()`])
    /// to the specified value.
    /// 
    /// Default is 1883 when TLS is **not** used and 8883 when TLS is **in use**.
    pub fn set_default_port(&mut self, port: u16) -> &mut Self
    {
        self.default_port = port;
        self.default_port_changed = true;

        self
    }

    /// Accesses the default port (the port used if no port is explicitly
    /// specified in the hostname passed to [`super::Client::connect()`]).
    /// 
    /// Default is 1883 when TLS is **not** used and 8883 when TLS is **in use**.
    pub fn default_port(&self) -> u16
    {
        self.default_port
    }

    pub(super) fn separate_connection_cfg(self) -> (OptionsT<'a, ()>, T)
    {
        let mut opt = self.map_connection_cfg(Some);
        let conn_cfg = opt.connection_cfg.take().unwrap();

        (opt.map_connection_cfg(|_| ()), conn_cfg)
    }
}

#[cfg(feature = "tls")]
mod tls {
    use std::sync::Arc;
    use std::io::{self, BufReader};

    use rustls::{ClientConfig, ClientConnection, RootCertStore, OwnedTrustAnchor, ServerName};
    use webpki::TrustAnchor;
    use tokio::net::TcpStream;

    use super::super::tls::{OptionsWithTls, CryptoBytes, CryptoError, Transport as TlsTransport};
    use super::super::errors::ConnectError;
    use super::super::transport::Transport;

    impl super::ConnectionConfig<ClientConnection> for ClientConfig
    {
        fn create_connection(self, host: &str) -> Result<ClientConnection, ConnectError>
        {
            let server_name = ServerName::try_from(host).map_err(|_| ConnectError::InvalidHostname)?;
            ClientConnection::new(Arc::new(self), server_name).map_err(ConnectError::TlsError)
        }

        fn create_transport(stream: TcpStream, connection: ClientConnection) -> Box<dyn Transport>
        {
            Box::new(TlsTransport::new(stream, connection))
        }
    }

    impl<'a> super::Options<'a>
    {
        /// Converts this set of options to options with TLS support using the
        /// specified TLS configuration. This specific function is for advanced uses
        /// and requires the `rustls` crate.
        /// 
        /// For simple uses, see [`Self::enable_tls()`] and [`Self::enable_tls_custom_ca_cert()`].
        pub fn enable_tls_ex(self, tls_config: ClientConfig) -> OptionsWithTls<'a>
        {
            let mut ret = self.map_connection_cfg(|_| tls_config);

            if !ret.default_port_changed {
                ret.default_port = 8883;
            }

            ret
        }

        /// Converts this set of options to options with TLS support. However,
        /// this specific function will **SKIP ALL SERVER AUTHENTICITY CHECKS**,
        /// meaning that the connection WILL **NOT** BE SECURE.
        /// 
        /// This is a **dangerous operation** and you should only use it for
        /// debug purposes if you know what you're doing. As a result, it is
        /// disabled by default, unless the `dangerous_tls` feature is enabled
        /// in the crate.
        /// 
        /// Please use [`Self::enable_tls()`] and [`Self::enable_tls_custom_ca_cert()`]
        /// instead.
        #[cfg(feature = "dangerous_tls")]
        pub fn enable_dangerous_non_verified_tls(self) -> OptionsWithTls<'a>
        {
            use super::super::tls::dangerous;

            let tls_config = ClientConfig::builder()
                .with_safe_defaults()
                .with_custom_certificate_verifier(Arc::new(dangerous::SkipServerCertVerification))
                .with_no_client_auth();
            
            self.enable_tls_ex(tls_config)
        }

        /// Converts this set of options to options with TLS support. This
        /// specific flavour loads the system's certificates to verify the
        /// server's identity.
        /// 
        /// If you would like to provide your own CA certificate, you can
        /// use the [`Self::enable_tls_custom_ca_cert()`] function.
        pub fn enable_tls(self) -> io::Result<OptionsWithTls<'a>>
        {
            let certs = rustls_native_certs::load_native_certs()?;
            let mut store = RootCertStore::empty();
            store.add_parsable_certificates(&certs.into_iter().map(|c| c.0).collect::<Vec<_>>());

            let tls_config = ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(store)
                .with_no_client_auth();

            Ok(self.enable_tls_ex(tls_config))
        }

        /// Converts this set of options to options with TLS support. This
        /// specific flavour enables the developer to provide their own
        /// CA certificate to verify the server's identity.
        /// 
        /// If you would like to use your OS's certificate store instead,
        /// use [`Self::enable_tls()`] instead.
        /// 
        /// # Parameters
        /// 
        /// `ca_cert_bytes` should contain a reference to the raw bytes of
        /// your certificate either in DER or PEM format. See [`CryptoBytes`]
        /// for more information.
        /// 
        /// # Return value
        /// 
        /// Because the bytes specified in `ca_cert_bytes` can be invalid,
        /// this function can fail. See [`CryptoError`] for more information.
        /// 
        /// # Example
        /// 
        /// ```
        /// use std::fs;
        /// use lmc::Options;
        /// use lmc::tls::CryptoBytes;
        /// 
        /// let cert_bytes = fs::read("test_data/ca.pem").expect("Failed to load CA certificate bytes");
        /// 
        /// let opts = Options::new("client_id")
        ///     .enable_tls_custom_ca_cert(CryptoBytes::Pem(&cert_bytes))
        ///     .expect("Failed to load the specified TLS certificate");
        /// ```
        pub fn enable_tls_custom_ca_cert(self, ca_cert_bytes: CryptoBytes) -> Result<OptionsWithTls<'a>, CryptoError>
        {
            let mut store = RootCertStore::empty();

            match ca_cert_bytes {
                CryptoBytes::Der(bytes) => {
                    let ca = TrustAnchor::try_from_cert_der(bytes).map_err(CryptoError::BadCert)?;
                    let owned_ca = OwnedTrustAnchor::from_subject_spki_name_constraints(
                        ca.subject,
                        ca.spki,
                        ca.name_constraints,
                    );

                    store.roots.push(owned_ca);
                },
                CryptoBytes::Pem(bytes) => {
                    let mut rd = BufReader::new(bytes);
                    let certs = rustls_pemfile::certs(&mut rd).map_err(CryptoError::IoError)?;

                    if store.add_parsable_certificates(&certs).0 < 1 {
                        return Err(CryptoError::NoValidItemsInPem);
                    }
                }
            }

            let tls_config = ClientConfig::builder()
                .with_safe_defaults()
                .with_root_certificates(store)
                .with_no_client_auth();

            Ok(self.enable_tls_ex(tls_config))
        }
    }
}
