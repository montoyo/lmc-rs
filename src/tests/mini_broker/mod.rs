use crate::QoS;
use crate::transport::{Transport, TcpTransport};
use crate::transceiver::packets::*;
use crate::transceiver::byte_io::ByteReader;

use std::io;
use std::net::{SocketAddrV4, Ipv4Addr};
use std::collections::VecDeque;
use std::time::Duration;
use std::panic::Location;
use std::sync::Arc;
use std::future::Future;

use tokio::net::TcpListener;
use tokio::time;
use rand::{thread_rng, Rng};
use log::{debug, trace};

mod sync_transport;
mod tracing;

use sync_transport::SyncTransport;
use tracing::{TracingUtility, KeyedTracingUtility};

pub struct MiniBroker
{
    transport: SyncTransport,
    pending_packets: VecDeque<IncomingBrokerPacket>,
    tracing_utility: KeyedTracingUtility,

    #[allow(dead_code)]
    listener: TcpListener
}

#[must_use] pub struct Expect<'a>(&'a mut MiniBroker);
#[must_use] pub struct ExpectOrEnqueue<'a>(&'a mut MiniBroker);

impl MiniBroker
{
    pub async fn create_listener() -> io::Result<(TcpListener, u16)>
    {
        let mut rng = thread_rng();
        let mut addr = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0);

        loop {
            addr.set_port(rng.gen_range(49152..=65535));
            debug!("Trying port {}...", addr.port());

            match TcpListener::bind(addr).await {
                Ok(x) => return Ok((x, addr.port())),
                Err(err) => match err.kind() {
                    io::ErrorKind::AddrInUse => {},
                    io::ErrorKind::PermissionDenied => {},
                    _ => return Err(err)
                }
            }
        }
    }

    pub fn new<T: Transport + 'static>(transport: T, listener: TcpListener) -> Self
    {
        let tracing_utility = TracingUtility::spawn();

        MiniBroker {
            transport: SyncTransport::new(transport, tracing_utility.clone()),
            pending_packets: VecDeque::new(),
            tracing_utility: tracing_utility.with_key("broker_task"),
            listener
        }
    }

    pub async fn new_tcp(listener: TcpListener) -> io::Result<Self>
    {
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;

        Ok(Self::new(TcpTransport::new(stream), listener))
    }

    #[cfg(feature = "tls")]
    pub async fn new_tls(listener: TcpListener) -> io::Result<Self>
    {
        use crate::tls::Transport as TlsTransport;
        use rustls::{ServerConfig, PrivateKey, Certificate, ServerConnection};
        use std::fs;

        let server_cert_bytes = fs::read("test_data/server.pem")?;
        let server_key_bytes = fs::read("test_data/server_private.pem")?;

        let mut server_cert_reader = io::BufReader::new(server_cert_bytes.as_slice());
        let mut server_key_reader = io::BufReader::new(server_key_bytes.as_slice());

        let cert_chain = rustls_pemfile::certs(&mut server_cert_reader)?.into_iter().map(Certificate).collect::<Vec<_>>();
        let private_key = match rustls_pemfile::read_one(&mut server_key_reader)?.unwrap() {
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
        let (stream, _) = listener.accept().await?;
        stream.set_nodelay(true)?;

        let transport = TlsTransport::new(stream, tls_conn);
        Ok(Self::new(transport, listener))
    }

    pub async fn send<P: Encode>(&mut self, pkt: &P) -> io::Result<()>
    {
        trace!("S==>C {:?}", P::packet_type());

        let data = pkt.make_packet();
        self.transport.write_fully(&data).await?;

        Ok(())
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
        trace!("S<==C {:?}", ret.packet_type());

        Ok(ret)
    }

    #[track_caller]
    pub fn recv_message(&mut self) -> impl Future<Output = io::Result<IncomingPublishPacket>> + '_
    {
        self.tracing_utility.trace("recv_message", Location::caller());
        self.recv_message_priv()
    }

    async fn recv_message_priv(&mut self) -> io::Result<IncomingPublishPacket>
    {
        self.tracing_utility.update_state("rx PUBLISH");
        let ret = self.expect_or_enqueue_no_track().publish().await?;
        let info = ret.info();
        let qos = info.flags.qos();

        if qos == QoS::AtLeastOnce {
            self.tracing_utility.update_state("tx PUBACK");
            self.send(&PubAckPacket::new(info.packet_id)).await?;
        } else if qos == QoS::ExactlyOnce {
            self.tracing_utility.update_state("tx PUBREC");
            self.send(&PubRecPacket::new(info.packet_id)).await?;

            self.tracing_utility.update_state("rx PUBREL");
            let rel = self.expect_or_enqueue_no_track().pubrel().await?;
            assert_eq!(rel.packet_id, info.packet_id);

            self.tracing_utility.update_state("tx PUBCOMP");
            self.send(&PubCompPacket::new(info.packet_id)).await?;
        }

        Ok(ret)
    }

    #[track_caller]
    pub fn send_message<'a>(&'a mut self, msg: &'a OutgoingPublishPacket<'a>) -> impl Future<Output = io::Result<()>> + 'a
    {
        self.tracing_utility.trace("send_message", Location::caller());
        self.send_message_priv(msg)
    }

    async fn send_message_priv(&mut self, msg: &OutgoingPublishPacket<'_>) -> io::Result<()>
    {
        self.tracing_utility.update_state("tx PUBLISH");
        self.send(msg).await?;

        if msg.flags.qos() == QoS::AtLeastOnce {
            self.tracing_utility.update_state("rx PUBACK");
            let ack = self.expect_or_enqueue_no_track().puback().await?;
            assert_eq!(msg.packet_id, ack.packet_id);
        } else if msg.flags.qos() == QoS::ExactlyOnce {
            self.tracing_utility.update_state("rx PUBREC");
            let rec = self.expect_or_enqueue_no_track().pubrec().await?;
            assert_eq!(msg.packet_id, rec.packet_id);

            self.tracing_utility.update_state("tx PUBREL");
            self.send(&PubRelPacket::new(msg.packet_id)).await?;

            self.tracing_utility.update_state("rx PUBCOMP");
            let comp = self.expect_or_enqueue_no_track().pubcomp().await?;
            assert_eq!(msg.packet_id, comp.packet_id);
        }

        Ok(())
    }

    #[track_caller]
    pub fn expect<'a>(&'a mut self) -> Expect<'a>
    {
        self.tracing_utility.trace("expecting", Location::caller());
        Expect(self)
    }
    
    #[track_caller]
    pub fn expect_or_enqueue<'a>(&'a mut self) -> ExpectOrEnqueue<'a>
    {
        self.tracing_utility.trace("expecting_or_enqueue", Location::caller());
        ExpectOrEnqueue(self)
    }

    fn expect_or_enqueue_no_track<'a>(&'a mut self) -> ExpectOrEnqueue<'a>
    {
        ExpectOrEnqueue(self)
    }

    pub async fn expect_disconnect(&mut self)
    {
        match self.recv(true).await {
            Ok(IncomingBrokerPacket::Disconnect(_)) => {
                let result = time::timeout(Duration::from_secs(10), self.transport.drop_byte()).await;

                match result {
                    Ok(Ok(true))  => {}, //Disconnected
                    Ok(Ok(false)) => panic!("Unexpected bytes after DISCONNECT packet."),
                    Ok(Err(err))  => panic!("Error while waiting for the socket to close: {:?}", err),
                    Err(_tiemout) => panic!("Timed out waiting for the socket to close")
                }
            },
            Ok(x) => panic!("Expected a disconnect packet, got a {:?} packet instead", x.packet_type()),
            Err(_) => {} //Clean disconnect is annoying to handle so we're just ignoring errors here...
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
                pub async fn $f(self) -> io::Result<$t>
                {
                    match self.0.recv(true).await? {
                        IncomingBrokerPacket::$v(x) => Ok(x),
                        x => panic!(concat!("Broker expected a ", stringify!($v), " packet, but got a {:?} packet instead!"), x.packet_type())
                    }
                }
            )+
        }

        impl<'a> ExpectOrEnqueue<'a>
        {
            $(
                pub async fn $f(self) -> io::Result<$t>
                {
                    let task = async move {
                        loop {
                            match self.0.recv(false).await? {
                                IncomingBrokerPacket::$v(x) => return Ok(x),
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
