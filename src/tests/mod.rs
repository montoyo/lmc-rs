use std::time::Duration;
use tokio::net::TcpListener;
use tokio::{time, join};
use log::debug;

mod mini_broker;
use mini_broker::MiniBroker;

use super::{Options, Client, QoS, PublishEvent, ClientShutdownHandle};
use super::transceiver::packets::*;

fn init_test()
{
    console_subscriber::init();

    let _ = env_logger::builder()
        //.is_test(true)
        .try_init();
}

async fn test_common(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
{
    debug!("==> Connected! session_present={}", client.was_session_present());

    let (mut sub, sub_qos) = client.subscribe_lossy(topic, QoS::ExactlyOnce, 10).await.unwrap();
    debug!("==> Successful subscription with qos {:?}", sub_qos);

    client.publish_qos_2(topic, b"is this working?", false, PublishEvent::Complete).await.unwrap();

    let msg = sub.recv().await.unwrap();
    debug!("==> Got message with packet ID {} in topic \"{}\" with QoS::{:?} with payload \"{}\"", msg.info().packet_id, msg.topic(), msg.info().flags.qos(), msg.payload_as_utf8().unwrap());
    drop(msg);

    time::sleep(Duration::from_secs(5)).await;
    drop(sub);

    time::sleep(Duration::from_secs(5)).await;
    client.publish_qos_1(topic, b"byebye1", false, true).await.unwrap();
    //We will only observe the Unsub at this point

    time::sleep(Duration::from_secs(5)).await;
    client.publish_qos_0(topic, b"byebye2", false).await.unwrap();
    drop(client);

    let disconnect_result = shutdown_handle.disconnect().await.unwrap();
    assert_eq!(disconnect_result.disconnect_sent, true);
    assert_eq!(disconnect_result.clean, true);
}

#[cfg(feature = "tls")]
#[tokio::test]
async fn test_tls()
{
    use std::fs;
    use super::tls;

    init_test();

    let mut opts = Options::new("test_tls")
        .enable_tls_custom_ca_cert(tls::CryptoBytes::Pem(&fs::read("./test_data/ca.pem").unwrap()))
        .unwrap();

    opts.enable_tls_client_auth(tls::CryptoBytes::Pem(&fs::read("./test_data/client.pem").unwrap()), tls::CryptoBytes::Pem(&fs::read("./test_data/client_private.pem").unwrap()))
        .unwrap()
        .set_last_will("test_tls", b"this is not good", false, QoS::AtLeastOnce)
        .set_keep_alive(10)
        .set_clean_session()
        .set_no_delay();
    
    let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
    test_common(client, shutdown_handle, "test_tls").await;
}

#[tokio::test]
async fn test_no_tls()
{
    init_test();

    let mut opts = Options::new("test_no_tls");

    opts.set_last_will("test_no_tls", b"this is not good", false, QoS::AtLeastOnce)
        .set_keep_alive(10)
        .set_clean_session()
        .set_no_delay();
    
    let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
    test_common(client, shutdown_handle, "test_no_tls").await;
}

async fn basic_test_broker(listener: TcpListener)
{
    let mut broker = MiniBroker::new_tcp(listener).await;

    broker.expect().connect().await;
    broker.send(&ConnAckPacket(Ok(false))).await;

    let sub = broker.expect().subscribe().await;
    let sub_qos = sub.topics[0].1;
    broker.send(&SubAckPacket::new(sub.packet_id, &[Ok(sub_qos)])).await;

    let msg1 = broker.recv_message().await;
    let msg2 = msg1.to_outgoing(sub_qos, false, 1);
    broker.send_message(&msg2).await;

    broker.expect().pingreq().await;
    broker.send(&PingRespPacket).await;

    debug!("Sent PingResp");

    let msg3 = broker.recv_message().await;
    debug!("Got message");
    let msg4 = msg3.to_outgoing(sub_qos, false, 2);
    broker.send_message(&msg4).await;
    debug!("Send Message");

    let unsub = broker.expect().unsub().await;
    debug!("Got unsub");
    broker.send(&UnsubAckPacket::new(unsub.packet_id)).await;

    broker.recv_message().await;
    broker.expect_disconnect().await;
}

#[tokio::test]
async fn test_mini_broker()
{
    init_test();

    let (listener, port) = MiniBroker::create_listener().await;
    let mut opts = Options::new("test_no_tls");

    opts.set_last_will("test_no_tls", b"this is not good", false, QoS::AtLeastOnce)
        .set_keep_alive(10)
        .set_clean_session()
        .set_no_delay()
        .disable_ipv6()
        .set_default_port(port);
    
    let broker_task = tokio::spawn(basic_test_broker(listener));
    let client_task = async move {
        let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
        test_common(client, shutdown_handle, "test_no_tls").await;
    };

    join!(broker_task, client_task).0.unwrap();
}
