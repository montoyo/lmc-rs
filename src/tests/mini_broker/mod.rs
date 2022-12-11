use crate::QoS;
use crate::transport::TcpTransport;
use crate::transceiver::packets::*;
use crate::transceiver::byte_io::ByteReader;

use std::io;
use std::net::{SocketAddrV4, Ipv4Addr};
use std::collections::VecDeque;
use std::time::Duration;
use std::panic::Location;
use std::sync::Arc;
use std::thread;
use std::future::Future;

use tokio::net::TcpListener;
use tokio::time;
use rand::{thread_rng, Rng};
use log::debug;
use parking_lot::Mutex;

mod sync_transport;
use sync_transport::SyncTransport;

#[derive(Clone, Copy)]
pub struct BlockInfo
{
    location: &'static Location<'static>,
    state: &'static str   
}

impl BlockInfo
{
    fn new() -> Self
    {
        Self {
            location: Location::caller(),
            state: "init"
        }
    }

    fn update(&mut self, location: &'static Location<'static>, state: &'static str)
    {
        self.location = location;
        self.state = state;
    }
}

pub struct MiniBroker
{
    transport: SyncTransport,
    pending_packets: VecDeque<IncomingBrokerPacket>,
    location: Arc<Mutex<BlockInfo>>,

    #[allow(dead_code)]
    listener: TcpListener
}

#[must_use] pub struct Expect<'a>(&'a mut MiniBroker);
#[must_use] pub struct ExpectOrEnqueue<'a>(&'a mut MiniBroker);

impl MiniBroker
{
    pub async fn create_listener() -> (TcpListener, u16)
    {
        let mut rng = thread_rng();
        let mut addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0);

        loop {
            addr.set_port(rng.gen_range(49152..=65535));
            debug!("Trying port {}...", addr.port());

            match TcpListener::bind(addr).await {
                Ok(x) => return (x, addr.port()),
                Err(err) => match err.kind() {
                    io::ErrorKind::AddrInUse => {},
                    io::ErrorKind::PermissionDenied => {},
                    _ => panic!("Could not listen: {:?}", err)
                }
            }
        }
    }

    fn new(transport: SyncTransport, listener: TcpListener) -> Self
    {
        let location = Arc::new(Mutex::new(BlockInfo::new()));
        let location_clone = Arc::downgrade(&location);
        debug!("Got client!");

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(10));

                let data = match location_clone.upgrade() {
                    Some(x) => x,
                    None => break
                };

                let cpy = *data.lock();
                debug!("Currently at {}:{}, waiting for {}", cpy.location.file(), cpy.location.line(), cpy.state);
            }
        });

        MiniBroker {
            transport,
            pending_packets: VecDeque::new(),
            location,
            listener
        }
    }

    pub async fn new_tcp(listener: TcpListener) -> Self
    {
        let (stream, _) = listener.accept().await.unwrap();
        Self::new(SyncTransport::new(TcpTransport::new(stream)), listener)
    }

    #[cfg(feature = "tls")]
    pub async fn new_tls(listener: TcpListener) -> MiniBroker
    {
        use crate::tls::Transport as TlsTransport;
        use rustls::{ServerConfig, PrivateKey, Certificate, ServerConnection};
        use std::fs;

        let server_cert_bytes = fs::read("test_data/server.pem").unwrap();
        let server_key_bytes = fs::read("test_data/server_private.pem").unwrap();

        let mut server_cert_reader = io::BufReader::new(server_cert_bytes.as_slice());
        let mut server_key_reader = io::BufReader::new(server_key_bytes.as_slice());

        let cert_chain = rustls_pemfile::certs(&mut server_cert_reader).unwrap().into_iter().map(Certificate).collect::<Vec<_>>();
        let private_key = match rustls_pemfile::read_one(&mut server_key_reader).unwrap().unwrap() {
            rustls_pemfile::Item::ECKey(der)    => PrivateKey(der),
            rustls_pemfile::Item::PKCS8Key(der) => PrivateKey(der),
            rustls_pemfile::Item::RSAKey(der)   => PrivateKey(der),
            _ => panic!()
        };
        
        let tls_cfg = ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_single_cert(cert_chain, private_key).unwrap();
            
        let tls_conn = ServerConnection::new(Arc::new(tls_cfg)).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let transport = TlsTransport::new(stream, tls_conn);

        Self::new(SyncTransport::new(transport), listener)
    }

    pub async fn send<P: Encode>(&mut self, pkt: &P)
    {
        let data = pkt.make_packet();
        self.transport.write_fully(&data).await.unwrap();
    }

    pub async fn recv(&mut self, use_pending: bool) -> io::Result<IncomingBrokerPacket>
    {
        if use_pending {
            if let Some(x) = self.pending_packets.pop_front() {
                return Ok(x);
            }
        }

        let data = self.transport.read_packet().await?;
        let mut rd = ByteReader::new(&data[1..]);
        let ret = IncomingBrokerPacket::from_bytes(&mut rd, ControlField(data[0])).unwrap();

        assert!(rd.remaining() <= 0, "Did not read {:?} packet in its entirety", ret.packet_type());
        Ok(ret)
    }

    #[track_caller]
    pub fn recv_message(&mut self) -> impl Future<Output = IncomingPublishPacket> + '_
    {
        self.location.lock().update(Location::caller(), "recv_message");
        self.recv_message_priv()
    }

    async fn recv_message_priv(&mut self) -> IncomingPublishPacket
    {
        self.location.lock().state = "rx PUBLISH";
        let ret = ExpectOrEnqueue(self).publish().await;
        let info = ret.info();
        let qos = info.flags.qos();

        if qos == QoS::AtLeastOnce {
            self.location.lock().state = "tx PUBACK";
            self.send(&PubAckPacket::new(info.packet_id)).await;
        } else if qos == QoS::ExactlyOnce {
            self.location.lock().state = "tx PUBREC";
            self.send(&PubRecPacket::new(info.packet_id)).await;

            self.location.lock().state = "rx PUBREL";
            let rel = ExpectOrEnqueue(self).pubrel().await;
            assert_eq!(rel.packet_id, info.packet_id);

            self.location.lock().state = "tx PUBCOMP";
            self.send(&PubCompPacket::new(info.packet_id)).await;
        }

        ret
    }

    #[track_caller]
    pub fn send_message<'a>(&'a mut self, msg: &'a OutgoingPublishPacket<'a>) -> impl Future<Output = ()> + 'a
    {
        self.location.lock().update(Location::caller(), "send_message");
        self.send_message_priv(msg)
    }

    async fn send_message_priv(&mut self, msg: &OutgoingPublishPacket<'_>)
    {
        self.location.lock().state = "tx PUBLISH";
        self.send(msg).await;

        if msg.flags.qos() == QoS::AtLeastOnce {
            self.location.lock().state = "rx PUBACK";
            let ack = ExpectOrEnqueue(self).puback().await;
            assert_eq!(msg.packet_id, ack.packet_id);
        } else if msg.flags.qos() == QoS::ExactlyOnce {
            self.location.lock().state = "rx PUBREC";
            let rec = ExpectOrEnqueue(self).pubrec().await;
            assert_eq!(msg.packet_id, rec.packet_id);

            self.location.lock().state = "tx PUBREL";
            self.send(&PubRelPacket::new(msg.packet_id)).await;

            self.location.lock().state = "rx PUBCOMP";
            let comp = ExpectOrEnqueue(self).pubcomp().await;
            assert_eq!(msg.packet_id, comp.packet_id);
        }
    }

    #[track_caller]
    pub fn expect<'a>(&'a mut self) -> Expect<'a>
    {
        self.location.lock().update(Location::caller(), "expecting");
        Expect(self)
    }
    
    #[track_caller]
    pub fn expect_or_enqueue<'a>(&'a mut self) -> ExpectOrEnqueue<'a>
    {
        self.location.lock().update(Location::caller(), "expecting_or_enqueue");
        ExpectOrEnqueue(self)
    }

    pub async fn expect_disconnect(&mut self)
    {
        match self.recv(true).await {
            Ok(IncomingBrokerPacket::Disconnect(_)) => {},
            Ok(x) => panic!("Expected a disconnect packet, got a {:?} packet instead", x.packet_type()),
            Err(_) => {}
        }
    }
}

impl Drop for MiniBroker
{
    fn drop(&mut self)
    {
        if self.pending_packets.len() > 0 {
            panic!("{} unprocessed packet on receive queue", self.pending_packets.len());
        }
    }
}

macro_rules! generate_expect
{
    { $(IncomingBrokerPacket::$v:ident($t:ty) => $f:ident),+ } => {
        impl<'a> Expect<'a>
        {
            $(
                pub async fn $f(self) -> $t
                {
                    match self.0.recv(true).await.unwrap() {
                        IncomingBrokerPacket::$v(x) => x,
                        x => panic!(concat!("Broker expected a ", stringify!($v), " packet, but got a {:?} packet instead!"), x.packet_type())
                    }
                }
            )+
        }

        impl<'a> ExpectOrEnqueue<'a>
        {
            $(
                pub async fn $f(self) -> $t
                {
                    let task = async move {
                        loop {
                            match self.0.recv(false).await.unwrap() {
                                IncomingBrokerPacket::$v(x) => return x,
                                x => self.0.pending_packets.push_back(x)
                            }
                        }
                    };

                    match time::timeout(Duration::from_secs(10), task).await {
                        Ok(x) => x,
                        Err(_) => panic!("Got stuck waiting for packet of type {:?}", PacketType::$v)
                    }
                }
            )+
        }
    };
}

generate_expect!
{
    IncomingBrokerPacket::Connect(IncomingConnectPacket)     => connect,
    IncomingBrokerPacket::Subscribe(IncomingSubscribePacket) => subscribe,
    IncomingBrokerPacket::Publish(IncomingPublishPacket)     => publish,
    IncomingBrokerPacket::PubAck(PubAckPacket)               => puback,
    IncomingBrokerPacket::PubRec(PubRecPacket)               => pubrec,
    IncomingBrokerPacket::PubRel(PubRelPacket)               => pubrel,
    IncomingBrokerPacket::PubComp(PubCompPacket)             => pubcomp,
    IncomingBrokerPacket::Unsubscribe(IncomingUnsubPacket)   => unsub,
    IncomingBrokerPacket::PingReq(PingReqPacket)             => pingreq
}
