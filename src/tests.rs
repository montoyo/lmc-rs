use std::time::Duration;
use tokio::time;

use super::{Options, Client, QoS, PublishEvent, ClientShutdownHandle};

fn init_test()
{
    let _ = env_logger::builder()
        .is_test(true)
        .filter(Some("lmc"), log::LevelFilter::Debug)
        .try_init();
}

async fn test_common(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
{
    log::debug!("==> Connected! session_present={}", client.was_session_present());

    let (mut sub, sub_qos) = client.subscribe_lossy(topic, QoS::ExactlyOnce, 10).await.unwrap();
    log::debug!("==> Successful subscription with qos {:?}", sub_qos);

    client.publish_qos_2(topic, b"is this working?", false, PublishEvent::Complete).await.unwrap();

    let msg = sub.recv().await.unwrap();
    log::debug!("==> Got message with packet ID {} in topic \"{}\" with QoS::{:?} with payload \"{}\"", msg.info().packet_id, msg.topic(), msg.info().flags.qos(), msg.payload_as_utf8().unwrap());
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
