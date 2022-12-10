use crate::transport::TcpTransport;
use crate::transceiver::packets::*;
use crate::transceiver::byte_io::ByteReader;

use std::io;
use tokio::net::TcpListener;

mod sync_transport;
use sync_transport::SyncTransport;

pub struct MiniBroker
{
    transport: SyncTransport
}

impl MiniBroker
{
    pub async fn new_tcp() -> MiniBroker
    {
        let listener = TcpListener::bind("127.0.0.1:1883").await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();

        MiniBroker { transport: SyncTransport::new(TcpTransport::new(stream)) }
    }

    #[cfg(feature = "tls")]
    pub async fn new_tls() -> MiniBroker
    {
        use crate::tls::Transport as TlsTransport;
        use rustls::{ServerConfig, PrivateKey, Certificate, ServerConnection};
        use std::fs;
        use std::sync::Arc;

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
        let listener = TcpListener::bind("127.0.0.1:1883").await.unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let transport = TlsTransport::new(stream, tls_conn);

        MiniBroker { transport: SyncTransport::new(transport) }
    }

    pub async fn send<P: Encode>(&mut self, pkt: &P)
    {
        let data = pkt.make_packet();
        self.transport.write_fully(&data).await.unwrap();
    }

    pub async fn recv(&mut self) -> IncomingBrokerPacket
    {
        let data = self.transport.read_packet().await.unwrap();
        let mut rd = ByteReader::new(&data[1..]);
        let ret = IncomingBrokerPacket::from_bytes(&mut rd, ControlField(data[0])).unwrap();

        assert!(rd.remaining() <= 0, "Did not read {:?} packet in its entirety", ret.packet_type());
        ret
    }
}

macro_rules! generate_expect
{
    { $(IncomingBrokerPacket::$v:ident($t:ty) => $f:ident),+ } => {
        impl MiniBroker {
            $(
                pub async fn $f(&mut self) -> $t
                {
                    match self.recv().await {
                        IncomingBrokerPacket::$v(x) => x,
                        x => panic!(concat!("Broker expected a ", stringify!($v), " packet, but got a {:?} packet instead!"), x.packet_type())
                    }
                }
            )+
        }
    };
}

generate_expect!
{
    IncomingBrokerPacket::Connect(IncomingConnectPacket)     => expect_connect,
    IncomingBrokerPacket::Subscribe(IncomingSubscribePacket) => expect_subscribe,
    IncomingBrokerPacket::Publish(IncomingPublishPacket)     => expect_publish,
    IncomingBrokerPacket::PubAck(PubAckPacket)               => expect_puback,
    IncomingBrokerPacket::PubRec(PubRecPacket)               => expect_pubrec,
    IncomingBrokerPacket::PubRel(PubRelPacket)               => expect_pubrel,
    IncomingBrokerPacket::PubComp(PubCompPacket)             => expect_pubcomp,
    IncomingBrokerPacket::Unsubscribe(IncomingUnsubPacket)   => expect_unsub,
    IncomingBrokerPacket::PingReq(PingReqPacket)             => expect_pingreq,
    IncomingBrokerPacket::Disconnect(DisconnectPacket)       => expect_disconnect
}
