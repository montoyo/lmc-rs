use std::io;
use std::fmt::{self, Formatter, Display};
use std::error::Error;
use std::str::Utf8Error;

/// An enumeration of all the possible error codes returned by
/// the broker in response to the client's `CONNECT` packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerConnectError
{
    /// The broker refused our request for the MQTT v3 protocol.
    UnacceptableProtocolVersion,

    /// The identifier specified by the developer has been
    /// was not allowed by the broker.
    IdentifierRejected,

    /// The broker responded but is currently unavailable.
    ServerUnavailable,

    /// The username and/or password data is malformed.
    BadUserNameOrPassword,

    /// The client is not authorized to connect.
    NotAuthorized,

    /// The broker returned an error code that is reserved for
    /// future use. The specific code used by the broker can be
    /// known by accessing this variant's value.
    Unknown(u8),

    /// This is not an official MQTT v3 error and is specific
    /// to this library. It usually means that the server the
    /// developer is trying to connect to is probably not an
    /// MQTT broker.
    ProtocolError
}

/// This enum specifies which operation in particular triggered
/// a timeout while trying to connect to the broker.
/// 
/// See [`ConnectError`]
#[derive(Debug)]
pub enum TimeoutKind
{
    /// The DNS lookup (host name resolving) timed out.
    DnsLookup,

    /// Establishing the TCP connection timed out.
    TcpConnect,

    /// The server did not reply to our `CONNACK` packet
    /// in time.
    MqttConnect
}

/// An enumeration of all the errors that could happen while
/// establishing a connection to the broker.
#[derive(Debug)]
pub enum ConnectError
{
    /// The specified hostname is not valid. This happens when
    /// TLS is in use and when the hostname is not acceptable
    /// for identity verfication purposes.
    InvalidHostname,

    /// A TLS error occurred while trying to establish a secure
    /// connection. See [`TlsError`].
    #[cfg(feature = "tls")]
    TlsError(rustls::Error),

    /// An IO error ocurred while trying to establish the
    /// connection. See [`io::Error`].
    IoError(io::Error),

    /// An IO error ocurred during the DNS lookup. See 
    /// [`io::Error`].
    LookupHostError(io::Error),

    /// Either the DNS query returned with no results, or the
    /// specified hostname does not have an IP address compatible
    /// with the Internet Protocol (IP) versions selected in
    /// [`super::options::OptionsT`].
    HostnameNotFound,

    /// One of the connection operation timed out. The value of
    /// this variant can be used to determine which operation
    /// timed out.
    /// 
    /// Note that all of these timeouts can be configured in
    /// [`super::options::OptionsT`].
    Timeout(TimeoutKind),

    /// This variant should never occur. It means that the client's
    /// transceiver task terminated without specifying a connection
    /// result.
    OneshotRecvError,

    /// The connection succeeded, but the server refused it explicitly.
    /// This variant's value can be used to determine the exact reason.
    ServerError(ServerConnectError)
}

/// An enumeration specifying what went wrong while trying to publish
/// a message using any of [`super::Client`]'s `publish` functions.
#[derive(Debug)]
pub enum PublishError
{
    /// The client's transceiver task terminated and thus the message
    /// might not have been transmitted to the broker.
    /// 
    /// This happens when [`super::Client::disconnect()`] or
    /// [`super::ClientShutdownHandle::disconnect()`] is called before
    /// the message could have been successfully transmitted by the client
    /// or acknowledged by the server.
    TransceiverTaskTerminated
}

/// An enumeration specifying what went wrong while trying to publish
/// a message using [`super::Client::try_publish`].
#[derive(Debug)]
pub enum TryPublishError
{
    /// The queue was full and thus the message was not sent.
    QueueFull,

    /// The client's transceiver task terminated and thus the message
    /// might not have been transmitted to the broker.
    /// 
    /// This happens when [`super::Client::disconnect()`] or
    /// [`super::ClientShutdownHandle::disconnect()`] is called before
    /// the message could have been successfully transmitted by the client
    /// or acknowledged by the server.
    TransceiverTaskTerminated
}

/// An enumeration specifying what went wrong while trying to subscribe
/// to a topic using any of [`super::Client`]'s `subscribe` functions.
#[derive(Debug)]
pub enum SubscribeError
{
    /// The broker denied the request to subscribe to the specified
    /// topic. No further information is provided.
    RefusedByBroker,

    /// The client's transceiver task terminated before the client
    /// could subscribe to a topic.
    /// 
    /// This happens when [`super::Client::disconnect()`] or
    /// [`super::ClientShutdownHandle::disconnect()`] is called before
    /// or while trying to subscribe to a topic.
    TransceiverTaskTerminated
}

/// Internal enum specifying why a packet couldn't be decoded.
#[derive(Debug)]
pub enum PacketDecodeError
{
    /// More bytes were expected while decoding the packet
    ReachedEndUnexpectedly,

    /// Found malformed UTF-8 while decoding a string contained
    /// in the packet.
    Utf8Error(Utf8Error),

    /// Invalid values were found in the packet.
    MalformedPacket,

    /// The size of the packet specified in the header is too
    /// high and thus the client preferred not to read it.
    ReachedMaxSize,

    /// The packet type field has an invalid value.
    InvalidPacketId(u8)
}

impl Display for PacketDecodeError
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result
    {
        match self {
            PacketDecodeError::ReachedEndUnexpectedly => write!(f, "reached end of packet unexpectedly"),
            PacketDecodeError::Utf8Error(_)           => write!(f, "invalid utf-8 found in packet"),
            PacketDecodeError::MalformedPacket        => write!(f, "packet contained invalid values"),
            PacketDecodeError::ReachedMaxSize         => write!(f, "maximum packet size reached"),
            PacketDecodeError::InvalidPacketId(pid)   => write!(f, "packet with id 0x{:02x} is invalid", pid),
        }
    }
}

impl Error for PacketDecodeError
{
    fn source(&self) -> Option<&(dyn Error + 'static)>
    {
        match self {
            PacketDecodeError::ReachedEndUnexpectedly => None,
            PacketDecodeError::Utf8Error(src)         => Some(src),
            PacketDecodeError::MalformedPacket        => None,
            PacketDecodeError::ReachedMaxSize         => None,
            PacketDecodeError::InvalidPacketId(_)     => None
        }
    }
}

impl Into<io::Error> for PacketDecodeError
{
    fn into(self) -> io::Error
    {
        io::Error::new(io::ErrorKind::Other, Box::new(self))
    }
}
