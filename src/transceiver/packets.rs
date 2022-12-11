use std::str::Utf8Error;
use std::mem::{size_of, MaybeUninit};
use std::sync::Arc;

use super::byte_io::{BigEndian, ByteReader, ByteWriter};
use super::super::QoS;
use super::super::options::LastWill;
use super::super::errors::{ServerConnectError, PacketDecodeError};

pub const MAX_HEADER_SIZE: usize = 5; //1 byte control field + up to 4 bytes of 'remaining length' field

/// Listing of the different MQTT packet types and their associated IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketType
{
    Connect     = 1,
    ConnAck     = 2,
    Publish     = 3,
    PubAck      = 4,
    PubRec      = 5,
    PubRel      = 6,
    PubComp     = 7,
    Subscribe   = 8,
    SubAck      = 9,
    Unsubscribe = 10,
    UnsubAck    = 11,
    PingReq     = 12,
    PingResp    = 13,
    Disconnect  = 14
}

#[derive(Clone, Copy, Debug)]
pub struct ControlField(pub u8);

impl ControlField
{
    pub const fn from_type_and_flags(t: PacketType, flags: u8) -> Self
    {
        if flags >= 16 {
            panic!("invalid flags");
        }

        Self(((t as u8) << 4) | flags)
    }

    pub const fn packet_type(self) -> u8
    {
        (self.0 & 0xf0) >> 4
    }

    pub const fn flags(self) -> u8
    {
        self.0 & 0x0f
    }
}

macro_rules! def_incoming_packets {
    { $(#[$($attrs:tt)*])* pub enum IncomingPacket { $($name:ident($type:ty)),+ } } => {
        $(#[$($attrs)*])*
        pub enum IncomingPacket
        {
            $($name($type)),+
        }

        impl IncomingPacket
        {
            /// Attempts to parse the bytes from the specified [`ByteReader`] into a packet
            /// struct that corresponds to the packet ID contained in the specified [`ControlField`].
            pub fn from_bytes(rd: &mut ByteReader, ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
            {
                match ctrl_field.packet_type() {
                    $(x if x == <$type>::packet_type() as u8 => <$type>::decode(rd, ctrl_field).map(Self::$name),)+
                    id => Err(PacketDecodeError::InvalidPacketId(id))
                }
            }

            /// Returns the [`PacketType`] of the underlying packet.
            pub fn packet_type(&self) -> PacketType
            {
                match self {
                    $(Self::$name(_) => <$type>::packet_type()),+
                }
            }
        }

        #[allow(dead_code)]
        #[doc(hidden)]
        const fn _check_packet_types()
        {
            $(assert!(<$type>::packet_type() as u8 == PacketType::$name as u8);)+
        }

        #[allow(dead_code)]
        #[doc(hidden)]
        const _: () = _check_packet_types();
    };
}

def_incoming_packets! {
    /// An enumeration of all the packets that can possibly be received
    /// by an MQTT client.
    pub enum IncomingPacket
    {
        ConnAck(ConnAckPacket),
        SubAck(SubAckPacket),
        Publish(IncomingPublishPacket),
        PubAck(PubAckPacket),
        PubRec(PubRecPacket),
        PubRel(PubRelPacket),
        PubComp(PubCompPacket),
        UnsubAck(UnsubAckPacket),
        PingResp(PingRespPacket)
    }
}

#[doc(hidden)]
#[cfg(test)]
mod test
{
    use super::*;

    def_incoming_packets! {
        pub enum IncomingPacket
        {
            Connect(IncomingConnectPacket),
            Subscribe(IncomingSubscribePacket),
            Publish(IncomingPublishPacket),
            PubAck(PubAckPacket),
            PubRec(PubRecPacket),
            PubRel(PubRelPacket),
            PubComp(PubCompPacket),
            Unsubscribe(IncomingUnsubPacket),
            PingReq(PingReqPacket),
            Disconnect(DisconnectPacket)
        }
    }
}

/// An enumeration of all the packets that can possibly be received
/// by an MQTT broker. This is only used in tests as LMC does not
/// provide any broker implementation.
#[cfg(test)]
pub use test::IncomingPacket as IncomingBrokerPacket;

fn encode_packet_size(mut sz: usize, dst: &mut [u8]) -> usize
{
    for i in 0..dst.len() {
        dst[i] = (sz & 0x7f) as u8;
        sz = (sz & !0x7f) >> 7;

        if sz <= 0 {
            return i + 1;
        }

        dst[i] |= 0x80;
    }

    panic!("packet too big");
}

/// A trait used to allocate packets directly on the heap in one,
/// single malloc.
/// 
/// Currently, this trait is implemented for [`Box`] and [`Arc`].
pub trait PacketContainer
{
    /// Creates a new, uninitialized container with the specified size
    fn create(len: usize) -> Self;

    /// Accesses the unitialized bytes as a mutable slice
    fn access_bytes(&mut self) -> &mut [MaybeUninit<u8>];
}

impl PacketContainer for Box<[MaybeUninit<u8>]>
{
    fn create(len: usize) -> Self
    {
        Box::new_uninit_slice(len)
    }

    fn access_bytes(&mut self) -> &mut [MaybeUninit<u8>]
    {
        self.as_mut()
    }
}

impl PacketContainer for Arc<[MaybeUninit<u8>]>
{
    fn create(len: usize) -> Self
    {
        Arc::new_uninit_slice(len)
    }

    fn access_bytes(&mut self) -> &mut [MaybeUninit<u8>]
    {
        unsafe {
            //SAFETY:
            // - This will be the only existing `Arc` instance at the time. This is somewhat guaranteed by the trait

            Arc::get_mut_unchecked(self)
        }
    }
}

/// A trait implemented by all packets. Simply associates
/// a [`PacketType`] to the struct.
pub trait Packet
{
    /// The type of packet that the implementing structure represents.
    fn packet_type() -> PacketType;
}

/// A trait implemented by all outgoing packets. Provides the
/// [`Encode::make_packet()`] and [`Encode::make_arc_packet()`]
/// utility functions to build packets (= byte arrays) wrapped
/// in [`Box`] and [`Arc`], respectively.
pub trait Encode: Packet
{
    /// Should return the size, in bytes, needed to encode this packet.
    /// 
    /// This value should not include the space needed for the fixed
    /// header (e.g. control field and packet size).
    fn compute_size(&self) -> usize;

    /// Should convert the packet into raw bytes using the provided
    /// [`ByteWriter`], excluding the fixed header (control field
    /// and packet size). Returns the flags in the control field that
    /// will be prepended to the packet by the caller.
    /// 
    /// Note that this method should write the **exact** number of
    /// bytes specified by the return value of [`Encode::compute_size()`].
    /// If this is not the case, the caller will panic.
    fn encode(&self, wr: &mut ByteWriter) -> u8;

    /// Encodes the packet into raw bytes and wraps the resulting byte
    /// array into the specified [`PacketContainer`]. This function
    /// should not be used directly and the [`Encode::make_packet`]
    /// and [`Encode::make_arc_packet`] utility functions should be
    /// used instead.
    fn make_packet_t<C: PacketContainer>(&self) -> C
    {
        let mut header = [0; MAX_HEADER_SIZE];
        let content_sz = self.compute_size();
        let header_sz = encode_packet_size(content_sz, &mut header[1..]) + 1;

        let mut ret = C::create(header_sz + content_sz);
        let bytes = ret.access_bytes();
        let mut wr = ByteWriter::new(&mut bytes[header_sz..]);
        let flags = self.encode(&mut wr);

        wr.finish_and_check();
        header[0] = ControlField::from_type_and_flags(Self::packet_type(), flags).0;
        
        unsafe {
            //SAFETY:
            // - [u8] is trivially copyable
            // - header_sz <= MAX_HEADER_SIZE
            // - bytes.len() >= header_sz

            std::ptr::copy_nonoverlapping(header.as_ptr().cast(), bytes.as_mut_ptr(), header_sz);
        }

        ret
    }

    /// Encodes the packet into raw bytes and wraps the resulting byte
    /// array into a new [`Box`].
    /// 
    /// This **used to be** preffered over [`Encode::make_arc_packet()`]
    /// for packets that would only be transmitted once, however the
    /// performance gain of using [`Box`] instead of [`Arc`] in this
    /// specific case is negligible, so this function is not used
    /// anymore, except in tests.
    fn make_packet(&self) -> Box<[u8]>
    {
        unsafe {
            //SAFTEY:
            // - In `make_packet_t`, we first initialized `bytes[header_sz..]` (this has been checked by `ByteWriter::finish_and_check`)
            // - In `make_packet_t`, we then initialized `bytes[..header_sz]`
            // - Upon return, the Box is fully initialized

            self.make_packet_t::<Box<[MaybeUninit<u8>]>>().assume_init()
        }
    }

    /// Encodes the packet into raw bytes and wraps the resulting byte
    /// array into a new [`Arc`].
    fn make_arc_packet(&self) -> Arc<[u8]>
    {
        unsafe {
            //SAFTEY:
            // - In `make_packet_t`, we first initialized `bytes[header_sz..]` (this has been checked by `ByteWriter::finish_and_check`)
            // - In `make_packet_t`, we then initialized `bytes[..header_sz]`
            // - Upon return, the Arc is fully initialized

            self.make_packet_t::<Arc<[MaybeUninit<u8>]>>().assume_init()
        }
    }
}

/**************************** CONNECT  ****************************/

mod connect_flags
{
    pub const USER_NAME    : u8 = 128;
    pub const PASSWORD     : u8 = 64;
    pub const WILL_RETAIN  : u8 = 32;
    pub const WILL_QOS_POS : u8 = 3;
    pub const WILL_FLAG    : u8 = 4;
    pub const CLEAN_SESSION: u8 = 2;
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct ConnectHeader
{
    protocol_name_len: BigEndian<u16>, // = 4
    protocol_name: [u8; 4],            // = b"MQTT"
    protocol_level: u8,                // = 4 for MQTT v3
    connect_flags: u8,                 //See 'ConnectFlags'
    keep_alive: BigEndian<u16>         //Keep alive interval in seconds
}

pub struct ConnectPacket<'a>
{
    pub clean_session: bool,
    pub keep_alive: u16,
    pub client_id: &'a str,
    pub will: Option<LastWill<'a>>,
    pub username: Option<&'a str>,
    pub password: Option<&'a [u8]>
}

/// Returns the number of bytes needed to encode the specified byte array (or string)
/// into an MQTT packet.
fn array_size<T: AsRef<[u8]>>(a: T) -> usize
{
    size_of::<u16>() + a.as_ref().len()
}

impl<'a> Packet for ConnectPacket<'a>
{
    fn packet_type() -> PacketType { PacketType::Connect }
}

impl<'a> Encode for ConnectPacket<'a>
{
    fn compute_size(&self) -> usize
    {
        let mut ret = size_of::<ConnectHeader>() + array_size(self.client_id);

        if let Some(will) = &self.will {
            ret += array_size(will.topic);
            ret += array_size(will.message);
        }

        if let Some(x) = self.username {
            ret += array_size(x);
        }

        if let Some(x) = self.password {
            ret += array_size(x);
        }

        ret
    }

    fn encode(&self, wr: &mut ByteWriter) -> u8
    {
        //Variable header
        let mut flags = 0;

        if self.clean_session {
            flags |= connect_flags::CLEAN_SESSION;
        }

        if let Some(will) = &self.will {
            flags |= connect_flags::WILL_FLAG;
            flags |= (will.qos as u8) << connect_flags::WILL_QOS_POS;

            if will.retain {
                flags |= connect_flags::WILL_RETAIN;
            }
        }

        if self.password.is_some() {
            flags |= connect_flags::PASSWORD;
        }

        if self.username.is_some() {
            flags |= connect_flags::USER_NAME;
        }

        let hdr = ConnectHeader {
            protocol_name_len: BigEndian::from_native(4),
            protocol_name: *b"MQTT",
            protocol_level: 4,
            connect_flags: flags,
            keep_alive: BigEndian::from_native(self.keep_alive)
        };

        wr.write_trivial(&hdr);

        //Payload
        wr.write_bytes(self.client_id);

        if let Some(will) = &self.will {
            wr.write_bytes(will.topic);
            wr.write_bytes(will.message);
        }

        if let Some(x) = self.username {
            wr.write_bytes(x);
        }

        if let Some(x) = self.password {
            wr.write_bytes(x);
        }

        0
    }
}

/// Last will structure for test broker. Only available in tests.
/// For more information regarding the last will, see [`LastWill`].
#[cfg(test)]
pub struct IncomingLastWill
{
    pub topic: String,
    pub message: Vec<u8>,
    pub retain: bool,
    pub qos: QoS
}

#[cfg(test)]
pub struct IncomingConnectPacket
{
    pub clean_session: bool,
    pub keep_alive: u16,
    pub client_id: String,
    pub will: Option<IncomingLastWill>,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>
}

#[cfg(test)]
impl const Packet for IncomingConnectPacket
{
    fn packet_type() -> PacketType { PacketType::Connect }
}

#[cfg(test)]
impl IncomingConnectPacket
{
    fn decode(rd: &mut ByteReader, ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        assert_eq!(ctrl_field.flags(), 0);

        let hdr = rd.read_trivial::<ConnectHeader>()?;
        assert_eq!(hdr.protocol_name_len.to_native(), 4);
        assert_eq!(&hdr.protocol_name, b"MQTT");
        assert_eq!(hdr.protocol_level, 4);

        let client_id = rd.read_utf8()?.to_string();

        let will = if (hdr.connect_flags & connect_flags::WILL_FLAG) == 0 {
            None
        } else {
            let topic = rd.read_utf8()?.to_string();
            let message = rd.read_byte_array()?.to_vec();

            Some(IncomingLastWill {
                topic, message,
                retain: (hdr.connect_flags & connect_flags::WILL_RETAIN) != 0,
                qos: match (hdr.connect_flags >> connect_flags::WILL_QOS_POS) & 3 {
                    0 => QoS::AtMostOnce,
                    1 => QoS::AtLeastOnce,
                    2 => QoS::ExactlyOnce,
                    _ => panic!()
                }
            })
        };

        let username = if (hdr.connect_flags & connect_flags::USER_NAME) == 0 { None } else { Some(rd.read_utf8()?.to_string()) };
        let password = if (hdr.connect_flags & connect_flags::PASSWORD) == 0 { None } else { Some(rd.read_byte_array()?.to_vec()) };

        Ok(IncomingConnectPacket {
            clean_session: (hdr.connect_flags & connect_flags::CLEAN_SESSION) != 0,
            keep_alive: hdr.keep_alive.to_native(),
            client_id, will, username, password
        })
    }
}

/**************************** CONNACK  ****************************/

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct ConnAckHeader
{
    ack_flags: u8,
    return_code: u8
}

pub struct ConnAckPacket(pub Result<bool, ServerConnectError>);

impl const Packet for ConnAckPacket
{
    fn packet_type() -> PacketType { PacketType::ConnAck }
}

impl ConnAckPacket
{
    fn decode(rd: &mut ByteReader, _ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        let hdr: ConnAckHeader = rd.read_trivial()?;

        Ok(ConnAckPacket(
            match hdr.return_code {
                0 => Ok((hdr.ack_flags & 1) != 0),
                1 => Err(ServerConnectError::UnacceptableProtocolVersion),
                2 => Err(ServerConnectError::IdentifierRejected),
                3 => Err(ServerConnectError::ServerUnavailable),
                4 => Err(ServerConnectError::BadUserNameOrPassword),
                5 => Err(ServerConnectError::NotAuthorized),
                x => Err(ServerConnectError::Unknown(x))
            }
        ))
    }
}

#[cfg(test)]
impl Encode for ConnAckPacket
{
    fn compute_size(&self) -> usize { size_of::<ConnAckHeader>() }

    fn encode(&self, wr: &mut ByteWriter) -> u8
    {
        let (ack_flags, return_code) = match self.0 {
            Ok(x) => (if x { 1 } else { 0 }, 0),
            Err(ServerConnectError::UnacceptableProtocolVersion) => (0, 1),
            Err(ServerConnectError::IdentifierRejected)          => (0, 2),
            Err(ServerConnectError::ServerUnavailable)           => (0, 3),
            Err(ServerConnectError::BadUserNameOrPassword)       => (0, 4),
            Err(ServerConnectError::NotAuthorized)               => (0, 5),
            Err(ServerConnectError::Unknown(x))                  => (0, x),
            Err(ServerConnectError::ProtocolError)               => panic!()
        };

        wr.write_trivial(&ConnAckHeader { ack_flags, return_code });
        0
    }
}

/**************************** PUBLISH  ****************************/

/// This struct allows the developer to create and parse valid publish
/// flags. See MQTT v3 protocol for more information.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublishFlags(u8);

impl PublishFlags
{
    /// The bit used to indicate a duplicate message
    pub const DUP: u8 = 8;

    /// Mask for the [`QoS`] bits
    pub const QOS_MASK: u8 = 6;

    /// The bit used for the retain flag
    pub const RETAIN: u8 = 1;

    /// Mask containing all bits that may consitute valid publish flags
    pub const ALL_BITS: u8 = Self::RETAIN | Self::QOS_MASK | Self::DUP;

    /// Instantiates a new set of publish flags with the specified values:
    ///  - `dup` indicates that the message was already sent by the client
    ///    before, but is about to be re-transmitted due to a lack of
    ///    acknowledgment from the broker
    ///  - `qos` indicates the quality of service that will be used to
    ///    **send** the message
    ///  - `retain` indicates that the message should be stored by the
    ///    broker and transmitted when clients subscribe to the corresponding
    ///    topic
    pub const fn new(dup: bool, qos: QoS, retain: bool) -> Self
    {
        let mut ret = (qos as u8) << 1;
        if dup { ret |= Self::DUP; }
        if retain { ret |= Self::RETAIN; }

        Self(ret)
    }

    /// Indicates that the message was a retained message that is being
    /// transmitted by the broker in response to a topic subscription.
    pub const fn was_retained(self) -> bool
    {
        (self.0 & Self::RETAIN) != 0
    }

    /// Indicates the quality of service used to **receive** the message.
    /// The client that sent the message initially might have used a
    /// different quality of service to transmit it.
    pub const fn qos(self) -> QoS
    {
        match (self.0 & Self::QOS_MASK) >> 1 {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => panic!()
        }
    }

    /// Indicates that this publish packet was already sent by the broker
    /// before, but was re-transmitted due to a lack of acknowledgment from
    /// the client.
    pub const fn is_dup(self) -> bool
    {
        (self.0 & Self::DUP) != 0
    }

    /// Converts the flags into their raw byte form.
    pub const fn into_inner(self) -> u8
    {
        self.0
    }
}

impl TryFrom<u8> for PublishFlags
{
    type Error = PacketDecodeError;

    /// Validates [`PublishFlags`] in their raw byte form. If
    /// invalid bits are detected,
    /// [`PacketDecodeError::MalformedPacket`] is returned.
    fn try_from(value: u8) -> Result<Self, PacketDecodeError>
    {
        if (value & !Self::ALL_BITS) != 0 {
            return Err(PacketDecodeError::MalformedPacket);
        }

        if ((value & Self::QOS_MASK) >> 1) > QoS::ExactlyOnce as u8 {
            return Err(PacketDecodeError::MalformedPacket);
        }

        Ok(Self(value))
    }
}

pub struct OutgoingPublishPacket<'a>
{
    pub flags: PublishFlags,
    pub topic: &'a str,
    pub packet_id: u16,
    pub payload: &'a [u8]
}

impl<'a> Packet for OutgoingPublishPacket<'a>
{
    fn packet_type() -> PacketType { PacketType::Publish }
}

impl<'a> Encode for OutgoingPublishPacket<'a>
{
    fn compute_size(&self) -> usize
    {
        let mut ret = array_size(self.topic);

        if self.flags.qos() >= QoS::AtLeastOnce {
            ret += size_of::<u16>();
        }

        ret + self.payload.len()
    }

    fn encode(&self, wr: &mut ByteWriter) -> u8
    {
        wr.write_bytes(self.topic);

        if self.flags.qos() >= QoS::AtLeastOnce {
            wr.write_u16(BigEndian::from_native(self.packet_id));
        }

        wr.write_bytes_no_len(self.payload);
        self.flags.into_inner()
    }
}

/// A message received from the broker. Its contents are wrapped in
/// an [`Arc`], so this value can be safely cloned with no extra
/// mallocs.
#[derive(Clone)]
pub struct IncomingPublishPacket(Arc<[u8]>);

/// A simple struct containing the flags used by the broker to
/// transmit the message, as well as the packet ID (if applicable).
#[derive(Clone, Copy)]
pub struct PublishPacketInfo
{
    /// The flags used by the broker to transmit the message to
    /// the client. This can be used to know if the message is
    /// a duplicate, if the message was retained and what quality
    /// of service was used to transmit it.
    /// 
    /// For more information, see [`PublishFlags`].
    pub flags: PublishFlags,

    /// The packet ID used by the broker for this message. Note
    /// that this value is invalid when using [`QoS::AtMostOnce`],
    /// and thus will be zero.
    pub packet_id: u16
}

#[derive(Clone, Copy)]
struct InternalPublishHeader
{
    topic_len: usize,
    info: PublishPacketInfo
}

impl const Packet for IncomingPublishPacket
{
    fn packet_type() -> PacketType { PacketType::Publish }
}

impl IncomingPublishPacket
{
    /// Returns basic information about this message (flags and packet ID).
    #[inline]
    pub fn info(&self) -> PublishPacketInfo
    {
        unsafe {
            //SAFETY:
            // - `IncomingPublishPacket` guarantees that `self.0` contains a `IncomingPublishPacket`

            std::ptr::addr_of!((*self.0.as_ptr().cast::<InternalPublishHeader>()).info).read_unaligned()
        }
    }

    /// Returns the topic of this message.
    #[inline]
    pub fn topic(&self) -> &str
    {
        unsafe {
            //SAFETY:
            // - `IncomingPublishPacket` guarantees that `self.0` contains a `IncomingPublishPacket`
            // - `IncomingPublishPacket` guarantees that the topic is at [start..start + topic_len]
            // - `IncomingPublishPacket` guarantees that the topic is valid UTF-8

            let topic_len = std::ptr::addr_of!((*self.0.as_ptr().cast::<InternalPublishHeader>()).topic_len).read_unaligned();
            let start = size_of::<InternalPublishHeader>();

            std::str::from_utf8_unchecked(&self.0[start..start + topic_len])
        }
    }

    /// Returns the payload (contents) of this message as raw bytes.
    #[inline]
    pub fn payload(&self) -> &[u8]
    {
        unsafe {
            //SAFETY:
            // - `IncomingPublishPacket` guarantees that `self.0` contains a `IncomingPublishPacket`
            // - `IncomingPublishPacket` guarantees that the payload is at [start..]

            let topic_len = std::ptr::addr_of!((*self.0.as_ptr().cast::<InternalPublishHeader>()).topic_len).read_unaligned();
            let start = size_of::<InternalPublishHeader>() + topic_len;
            
            &self.0[start..]
        }
    }

    /// Verifies that the payload (contents) of this message is valid UTF-8
    /// and returns it as a [`str`] if that is the case. Returns an [`Utf8Error`]
    /// if it contains malformed UTF-8.
    #[inline]
    pub fn payload_as_utf8(&self) -> Result<&str, Utf8Error>
    {
        std::str::from_utf8(self.payload())
    }

    fn decode(rd: &mut ByteReader, ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        let flags = PublishFlags::try_from(ctrl_field.flags())?;
        let topic = rd.read_utf8()?;

        let packet_id = if flags.qos() >= QoS::AtLeastOnce {
            rd.read_u16()?.to_native()
        } else {
            0
        };
        
        let payload = rd.read_remaining();
        let hdr_size = size_of::<InternalPublishHeader>();
        let arc_size = hdr_size + topic.len() + payload.len();

        unsafe {
            let mut uninit_arc = Arc::<[u8]>::new_uninit_slice(arc_size);
            let uninit_arc_data = Arc::get_mut_unchecked(&mut uninit_arc).as_mut_ptr();

            std::ptr::write_unaligned(uninit_arc_data.cast(), InternalPublishHeader {
                topic_len: topic.len(),
                info: PublishPacketInfo { flags, packet_id }
            });

            std::ptr::copy_nonoverlapping(topic.as_ptr(), uninit_arc_data.add(hdr_size).cast(), topic.len());
            std::ptr::copy_nonoverlapping(payload.as_ptr(), uninit_arc_data.add(hdr_size + topic.len()).cast(), payload.len());

            Ok(IncomingPublishPacket(uninit_arc.assume_init()))
        }
    }

    #[cfg(test)]
    pub fn to_outgoing<'a>(&'a self, qos: QoS, retain: bool, packet_id: u16) -> OutgoingPublishPacket<'a>
    {
        OutgoingPublishPacket {
            flags: PublishFlags::new(false, qos, retain),
            topic: self.topic(),
            packet_id,
            payload: self.payload()
        }
    }
}

#[cfg(test)]
fn gen_publish_packet(dup: bool, qos: QoS, retain: bool) -> IncomingPublishPacket
{
    let flags = PublishFlags::new(dup, qos, retain);
    let packet_id = 1234u16;

    let pkt = OutgoingPublishPacket {
        flags, packet_id,
        topic: "hello there",
        payload: b"general kenobi"
    }.make_arc_packet();

    let mut rd = ByteReader::new(&pkt[2..]);
    IncomingPublishPacket::decode(&mut rd, ControlField::from_type_and_flags(PacketType::Publish, flags.0)).unwrap()
}

#[test]
fn test_incoming_pkt()
{
    let qos0 = gen_publish_packet(true, QoS::AtMostOnce, false);
    let qos1 = gen_publish_packet(false, QoS::AtLeastOnce, true);

    assert_eq!(qos0.info().flags.is_dup(), true);
    assert_eq!(qos0.info().flags.qos(), QoS::AtMostOnce);
    assert_eq!(qos0.info().flags.was_retained(), false);
    assert_eq!(qos0.info().packet_id, 0);
    assert_eq!(qos0.topic(), "hello there");
    assert_eq!(qos0.payload(), b"general kenobi");

    assert_eq!(qos1.info().flags.is_dup(), false);
    assert_eq!(qos1.info().flags.qos(), QoS::AtLeastOnce);
    assert_eq!(qos1.info().flags.was_retained(), true);
    assert_eq!(qos1.info().packet_id, 1234u16);
    assert_eq!(qos1.topic(), "hello there");
    assert_eq!(qos1.payload(), b"general kenobi");
}

/**************************** PUBACK, PUBREC, PUBREL, PUBCOMP, UNSUBACK  ****************************/

macro_rules! if_out {
    { in_out; $($tt:tt)+ } => { $($tt)+ };
    { in; $($tt:tt)+ } => { #[cfg(test)] $($tt)+ };
}

macro_rules! def_simple_packet {
    ($name:ident, $type:expr, flags = $flags:literal, direction = $dir:ident) => {
        pub struct $name
        {
            pub packet_id: u16
        }

        impl const Packet for $name
        {
            fn packet_type() -> PacketType { $type }
        }

        impl $name
        {
            if_out! {
                $dir;
                pub fn new(packet_id: u16) -> Self
                {
                    Self { packet_id }
                }
            }

            fn decode(rd: &mut ByteReader, _ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
            {
                rd.read_u16().map(|x| Self { packet_id: x.to_native() })
            }
        }

        if_out! {
            $dir;
            impl Encode for $name
            {
                fn compute_size(&self) -> usize
                {
                    size_of::<u16>()
                }
    
                fn encode(&self, wr: &mut ByteWriter) -> u8
                {
                    wr.write_u16(BigEndian::from_native(self.packet_id));
                    $flags
                }
            }
        }
    };
}

def_simple_packet!(PubAckPacket,   PacketType::PubAck,   flags = 0, direction = in_out);
def_simple_packet!(PubRecPacket,   PacketType::PubRec,   flags = 0, direction = in_out);
def_simple_packet!(PubRelPacket,   PacketType::PubRel,   flags = 2, direction = in_out);
def_simple_packet!(PubCompPacket,  PacketType::PubComp,  flags = 0, direction = in_out);
def_simple_packet!(UnsubAckPacket, PacketType::UnsubAck, flags = 0, direction = in);

/**************************** SUBSCRIBE  ****************************/

pub struct SubscribePacket<'a>
{
    pub packet_id: u16,
    pub topics: &'a [(&'a str, QoS)]
}

impl<'a> Packet for SubscribePacket<'a>
{
    fn packet_type() -> PacketType { PacketType::Subscribe }
}

impl<'a> Encode for SubscribePacket<'a>
{
    fn compute_size(&self) -> usize
    {
        let mut ret = size_of::<u16>();

        for &(topic, _) in self.topics {
            ret += array_size(topic);
            ret += size_of::<u8>();
        }

        ret
    }

    fn encode(&self, wr: &mut ByteWriter) -> u8
    {
        wr.write_u16(BigEndian::from_native(self.packet_id));

        for &(topic, qos) in self.topics {
            wr.write_bytes(topic).write_u8(qos as u8);
        }

        2
    }
}

#[cfg(test)]
pub struct IncomingSubscribePacket
{
    pub packet_id: u16,
    pub topics: Vec<(String, QoS)>
}

#[cfg(test)]
impl const Packet for IncomingSubscribePacket
{
    fn packet_type() -> PacketType { PacketType::Subscribe }
}

#[cfg(test)]
impl IncomingSubscribePacket
{
    fn decode(rd: &mut ByteReader, ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        assert_eq!(ctrl_field.flags(), 2);

        let mut ret = Self {
            packet_id: rd.read_u16()?.to_native(),
            topics: Vec::new()
        };

        while rd.remaining() > 0 {
            let topic = rd.read_utf8()?.to_string();
            let qos = match rd.read_u8()? {
                0 => QoS::AtMostOnce,
                1 => QoS::AtLeastOnce,
                2 => QoS::ExactlyOnce,
                _ => panic!()
            };

            ret.topics.push((topic, qos));
        }

        Ok(ret)
    }
}

/**************************** SUBACK  ****************************/

pub type SubscriptionResult = Result<QoS, ()>;
const MAX_STATIC_SUBS: usize = (size_of::<Vec<SubscriptionResult>>() - size_of::<usize>()) / size_of::<SubscriptionResult>();

/// A list [`SubscriptionResult`] that is allocated either statically (using a
/// fixed-size slice) or dynamically (using a [`Vec`]). This prevents unncessary
/// memory allocations as [`SubscriptionResult`]s are rather small (1 byte) and
/// most of the time a [`SubAckPacket`] only contains a few values.
enum SubscriptionResultList
{
    Static {
        len: usize,
        values: [SubscriptionResult; MAX_STATIC_SUBS]
    },
    Dynamic(Vec<SubscriptionResult>)
}

#[test]
fn print_sizes()
{
    println!("sizeof(SubscriptionResult) = {}", size_of::<SubscriptionResult>()); //1 byte, surprisingly
    println!("sizeof(Vec<u8>) = {}", size_of::<Vec<u8>>());
    println!("MAX_STATIC_SUBS = {}", MAX_STATIC_SUBS);
    println!("sizeof(SubscriptionResultList) = {}", size_of::<SubscriptionResultList>());
}

pub struct SubAckPacket
{
    pub packet_id: u16,
    sub_results: SubscriptionResultList
}

impl const Packet for SubAckPacket
{
    fn packet_type() -> PacketType { PacketType::SubAck }
}

impl SubAckPacket
{
    #[cfg(test)]
    pub fn new(packet_id: u16, results: &[SubscriptionResult]) -> Self
    {
        let sub_results = if results.len() <= MAX_STATIC_SUBS {
            let mut values = [Err(()); MAX_STATIC_SUBS];
            values[..results.len()].copy_from_slice(results);

            SubscriptionResultList::Static { len: results.len(), values }
        } else {
            SubscriptionResultList::Dynamic(results.to_vec())
        };

        Self { packet_id, sub_results }
    }

    pub fn sub_results(&self) -> &[SubscriptionResult]
    {
        match &self.sub_results {
            SubscriptionResultList::Static { len, values } => &values[..*len],
            SubscriptionResultList::Dynamic(d) => &d
        }
    }
}

fn parse_sub_result(r: u8) -> Result<SubscriptionResult, PacketDecodeError>
{
    match r {
        0   => Ok(Ok(QoS::AtMostOnce)),
        1   => Ok(Ok(QoS::AtLeastOnce)),
        2   => Ok(Ok(QoS::ExactlyOnce)),
        128 => Ok(Err(())),
        _   => Err(PacketDecodeError::MalformedPacket)
    }
}

impl SubAckPacket
{
    fn decode(rd: &mut ByteReader, _ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        let packet_id = rd.read_u16()?.to_native();
        let num_results = rd.remaining();

        let sub_results = if num_results <= MAX_STATIC_SUBS {
            let mut values = [Err(()); MAX_STATIC_SUBS];

            for (i, &r) in rd.read_remaining().iter().enumerate() {
                values[i] = parse_sub_result(r)?;
            }

            SubscriptionResultList::Static { len: num_results, values }
        } else {
            let mut values = Vec::with_capacity(num_results);

            for &r in rd.read_remaining() {
                values.push(parse_sub_result(r)?);
            }

            SubscriptionResultList::Dynamic(values)
        };

        Ok(SubAckPacket { packet_id, sub_results })
    }
}

#[cfg(test)]
impl Encode for SubAckPacket
{
    fn compute_size(&self) -> usize
    {
        match &self.sub_results {
            SubscriptionResultList::Static { len, values: _ } => len + size_of::<u16>(),
            SubscriptionResultList::Dynamic(x) => x.len() + size_of::<u16>()
        }
    }

    fn encode(&self, wr: &mut ByteWriter) -> u8
    {
        wr.write_u16(BigEndian::from_native(self.packet_id));

        for result in self.sub_results() {
            match result {
                Ok(qos) => drop(wr.write_u8(*qos as u8)),
                Err(()) => drop(wr.write_u8(128))
            }
        }

        0
    }
}

/**************************** UNSUBSCRIBE  ****************************/

pub struct UnsubscribePacket<'a>
{
    pub packet_id: u16,
    pub topics: &'a [&'a str]
}

impl<'a> Packet for UnsubscribePacket<'a>
{
    fn packet_type() -> PacketType { PacketType::Unsubscribe }
}

impl<'a> Encode for UnsubscribePacket<'a>
{
    fn compute_size(&self) -> usize
    {
        let mut ret = size_of::<u16>();

        for topic in self.topics {
            ret += array_size(topic);
        }

        ret
    }

    fn encode(&self, wr: &mut ByteWriter) -> u8
    {
        wr.write_u16(BigEndian::from_native(self.packet_id));

        for &topic in self.topics {
            wr.write_bytes(topic);
        }

        2
    }
}

#[cfg(test)]
pub struct IncomingUnsubPacket
{
    pub packet_id: u16,
    pub topics: Vec<String>
}

#[cfg(test)]
impl const Packet for IncomingUnsubPacket
{
    fn packet_type() -> PacketType { PacketType::Unsubscribe }
}

#[cfg(test)]
impl IncomingUnsubPacket
{
    fn decode(rd: &mut ByteReader, ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        assert_eq!(ctrl_field.flags(), 2);

        let mut ret = Self {
            packet_id: rd.read_u16()?.to_native(),
            topics: Vec::new()
        };

        while rd.remaining() > 0 {
            ret.topics.push(rd.read_utf8()?.to_string());
        }

        Ok(ret)
    }
}

/**************************** PINGREQ, DISCONNECT  ****************************/

macro_rules! def_empty_outgoing_packet {
    ($name:ident, $type:expr) => {
        pub struct $name;

        impl const Packet for $name
        {
            fn packet_type() -> PacketType { $type }
        }

        impl Encode for $name
        {
            fn compute_size(&self) -> usize
            {
                0
            }

            fn encode(&self, _wr: &mut ByteWriter) -> u8
            {
                0
            }
        }

        #[cfg(test)]
        impl $name
        {
            fn decode(rd: &mut ByteReader, ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
            {
                assert_eq!(ctrl_field.flags(), 0);
                Ok(Self)
            }
        }
    };
}

def_empty_outgoing_packet!(PingReqPacket, PacketType::PingReq);
def_empty_outgoing_packet!(DisconnectPacket, PacketType::Disconnect);

/**************************** PINGRESP  ****************************/

pub struct PingRespPacket;

impl const Packet for PingRespPacket
{
    fn packet_type() -> PacketType { PacketType::PingResp }
}

impl PingRespPacket
{
    fn decode(_rd: &mut ByteReader, _ctrl_field: ControlField) -> Result<Self, PacketDecodeError>
    {
        Ok(Self)
    }
}

#[cfg(test)]
impl Encode for PingRespPacket
{
    fn compute_size(&self) -> usize
    {
        0
    }

    fn encode(&self, _wr: &mut ByteWriter) -> u8
    {
        0
    }
}
