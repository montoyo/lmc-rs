use std::time::Duration;
use std::{env, io};
use std::sync::Once;

use tokio::{time, join, sync::mpsc::error::TryRecvError};
use log::{debug, error, LevelFilter};

mod mini_broker;
use mini_broker::MiniBroker;

use crate::{Options, Client, QoS, PublishEvent, ClientShutdownHandle, SubscriptionStatus};
use crate::transceiver::packets::*;

static TEST_INIT: Once = Once::new();

/// Initializes the logger and Tokio's console_subscriber.
/// This must be called at the beginning of each tests.
fn init_test()
{
    TEST_INIT.call_once(|| {
        console_subscriber::init();

        let mut logger_cfg = env_logger::builder();
    
        if !env::args_os().any(|arg| arg == "--nocapture") {
            logger_cfg.is_test(true);
        }
    
        if env::var_os("RUST_LOG").map(|x| x.is_empty()).unwrap_or(true) {
            logger_cfg.filter_module("lmc", LevelFilter::Debug);
        }
    
        if env::var_os("RUST_BACKTRACE").map(|x| x.is_empty()).unwrap_or(true) {
            env::set_var("RUST_BACKTRACE", "1");
        }
    
        let _ = logger_cfg.try_init();
    });
}

/// Generates four high-level tests from a client & broker functions:
///  - One using a third-party broker (presumed to be running on localhost:1883), without TLS
///  - One using a third-party broker (presumed to be running on localhost:8883), with TLS
///  - One using [`MiniBroker`] and the provided broker function, without TLS
///  - One using [`MiniBroker`] and the provided broker function, with TLS
/// 
/// See existing tests for examples.
macro_rules! def_tests
{
    {
        $(#[$($attrs:tt)*])*
        $name:ident:
        async fn client_fn($client:ident: Client, $shutdown_handle:ident: ClientShutdownHandle, $topic:ident: &str) $client_fn:block
        async fn broker_fn(mut $broker:ident: MiniBroker) -> io::Result<()> $broker_fn:block
    } =>
    {
        $(#[$($attrs)*])*
        mod $name
        {
            use super::*;
            async fn client_fn($client: Client, $shutdown_handle: ClientShutdownHandle, $topic: &str) $client_fn
            async fn broker_fn(mut $broker: MiniBroker) -> io::Result<()> $broker_fn

            #[tokio::test]
            async fn test_no_tls()
            {
                init_test();

                let test_name = concat!(stringify!($name), "_no_tls");
                let mut opts = Options::new(test_name);

                opts.set_last_will(test_name, b"this is not good", false, QoS::AtLeastOnce)
                    .set_keep_alive(10)
                    .set_clean_session()
                    .set_no_delay();
                
                let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
                client_fn(client, shutdown_handle, test_name).await;
            }

            #[cfg(feature = "tls")]
            #[tokio::test]
            async fn test_tls()
            {
                use crate::tls;
                use std::fs;

                init_test();

                let test_name = concat!(stringify!($name), "_tls");
                let mut opts = Options::new(test_name)
                    .enable_tls_custom_ca_cert(tls::CryptoBytes::Pem(&fs::read("./test_data/ca.pem").unwrap()))
                    .unwrap();

                opts.enable_tls_client_auth(tls::CryptoBytes::Pem(&fs::read("./test_data/client.pem").unwrap()), tls::CryptoBytes::Pem(&fs::read("./test_data/client_private.pem").unwrap()))
                    .unwrap()
                    .set_last_will(test_name, b"this is not good", false, QoS::AtLeastOnce)
                    .set_keep_alive(10)
                    .set_clean_session()
                    .set_no_delay();
                
                let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
                client_fn(client, shutdown_handle, test_name).await;
            }

            #[tokio::test]
            async fn test_mb_no_tls()
            {
                init_test();

                let (listener, port) = MiniBroker::create_listener().await.unwrap();
                let test_name = concat!(stringify!($name), "_mb_no_tls");
                let mut opts = Options::new(test_name);

                opts.set_last_will(test_name, b"this is not good", false, QoS::AtLeastOnce)
                    .set_keep_alive(8)
                    .set_clean_session()
                    .set_no_delay() //Required for tests with `MiniBroker`
                    .disable_ipv6()
                    .set_packets_resend_delay(Duration::from_secs(2)) //Required for tests with `MiniBroker`
                    .set_default_port(port);
                
                let broker_task = async move {
                    let ret = match MiniBroker::new_tcp(listener).await {
                        Ok(broker) => broker_fn(broker).await,
                        Err(err) => Err(err)
                    };

                    match ret {
                        Ok(()) => {
                            debug!("Broker task finished successfully!");
                            true
                        },
                        Err(err) => {
                            error!("Broker task failed: {:?}", err);
                            false
                        }
                    }
                };

                let client_task = async move {
                    let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
                    client_fn(client, shutdown_handle, test_name).await;
                    debug!("Client test task finished successfully!");
                };

                let (broker_ok, _) = join!(broker_task, client_task);
                assert!(broker_ok, "Broker task failed.");
            }

            #[cfg(feature = "tls")]
            #[tokio::test]
            async fn test_mb_tls()
            {
                use crate::tls;
                use std::fs;

                init_test();

                let (listener, port) = MiniBroker::create_listener().await.unwrap();
                let test_name = concat!(stringify!($name), "_mb_tls");

                let mut opts = Options::new(test_name)
                    .enable_tls_custom_ca_cert(tls::CryptoBytes::Pem(&fs::read("./test_data/ca.pem").unwrap()))
                    .unwrap();

                opts.set_last_will(test_name, b"this is not good", false, QoS::AtLeastOnce)
                    .set_keep_alive(8)
                    .set_clean_session()
                    .set_no_delay() //Required for tests with `MiniBroker`
                    .disable_ipv6()
                    .set_packets_resend_delay(Duration::from_secs(2)) //Required for tests with `MiniBroker`
                    .set_default_port(port);
                
                let broker_task = async move {
                    let ret = match MiniBroker::new_tls(listener).await {
                        Ok(broker) => broker_fn(broker).await,
                        Err(err) => Err(err)
                    };

                    match ret {
                        Ok(()) => {
                            debug!("Broker task finished successfully!");
                            true
                        },
                        Err(err) => {
                            error!("Broker task failed: {:?}", err);
                            false
                        }
                    }
                };

                let client_task = async move {
                    let (client, shutdown_handle) = Client::connect("localhost", opts).await.unwrap();
                    client_fn(client, shutdown_handle, test_name).await;
                    debug!("Client test task finished successfully!");
                };

                let (broker_ok, _) = join!(broker_task, client_task);
                assert!(broker_ok, "Broker task failed.");
            }
        }
    };
}

def_tests! {
    /// Simple test validating that `PINGREQ` packets are sent by the client after
    /// the configured amount of time.
    ping:
    
    async fn client_fn(client: Client, shutdown_handle: ClientShutdownHandle, _topic: &str)
    {
        debug!("==> Connected! session_present={}", client.was_session_present());
        time::sleep(Duration::from_secs(10)).await;
        drop(client);
    
        let disconnect_result = shutdown_handle.disconnect().await.unwrap();
        assert_eq!(disconnect_result.disconnect_sent, true);
        assert_eq!(disconnect_result.clean, true);
    }

    async fn broker_fn(mut broker: MiniBroker) -> io::Result<()>
    {
        broker.expect().connect().await?;
        broker.send(&ConnAckPacket(Ok(false))).await?;

        broker.expect().pingreq().await?;
        broker.send(&PingRespPacket).await?;

        broker.expect_disconnect().await;
        Ok(())
    }
}

def_tests! {
    /// Validates "void" subscription.
    /// 
    /// This test used to be more meaningful when auto-unsub was still part of LMC and
    /// "void" subscriptions were called "hold" subscriptions. Now, we're just keeping
    /// it as another test.
    sub_hold:
    
    async fn client_fn(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
    {
        debug!("==> Connected! session_present={}", client.was_session_present());
        client.subscribe_void(topic.to_string(), QoS::AtMostOnce).await.unwrap();
        assert_eq!(client.get_subscription_status(topic), SubscriptionStatus::Live);

        let (sub, qos) = client.subscribe_lossy(topic, QoS::ExactlyOnce, 5).await.unwrap();
        assert_eq!(qos, QoS::AtMostOnce);
        assert_eq!(client.get_subscription_status(topic), SubscriptionStatus::Live);
        drop(sub);

        time::sleep(Duration::from_secs(1)).await;

        client.publish_qos_0(topic, b"trigger unsub", false).await.unwrap();
        time::sleep(Duration::from_secs(3)).await;
        assert_eq!(client.get_subscription_status(topic), SubscriptionStatus::Live);

        client.unsubscribe(topic.to_string()).await;
        time::sleep(Duration::from_secs(1)).await;
        assert_eq!(client.get_subscription_status(topic), SubscriptionStatus::Absent);
        drop(client);
    
        let disconnect_result = shutdown_handle.disconnect().await.unwrap();
        assert_eq!(disconnect_result.disconnect_sent, true);
        assert_eq!(disconnect_result.clean, true);
    }

    async fn broker_fn(mut broker: MiniBroker) -> io::Result<()>
    {
        broker.expect().connect().await?;
        broker.send(&ConnAckPacket(Ok(false))).await?;

        let sub = broker.expect().subscribe().await?;
        let sub_qos = sub.topics[0].1;
        broker.send(&SubAckPacket::new(sub.packet_id, &[Ok(sub_qos)])).await?;

        let msg1 = broker.recv_message().await?;
        let msg2 = msg1.to_outgoing(sub_qos, false, 1);
        broker.send_message(&msg2).await?;

        let unsub = broker.expect().unsub().await?;
        broker.send(&UnsubAckPacket::new(unsub.packet_id)).await?;

        broker.expect_disconnect().await;
        Ok(())
    }
}


def_tests! {
    /// Validates fast callback subscriptions.
    fast_callback:
    
    async fn client_fn(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
    {
        use tokio::sync::oneshot;
        debug!("==> Connected! session_present={}", client.was_session_present());

        let (os_tx, os_rx) = oneshot::channel::<IncomingPublishPacket>();
        let mut os_tx = Some(os_tx);

        let (sub, qos) = client
            .subscribe_fast_callback(topic.to_string(), QoS::AtMostOnce, move |msg| os_tx.take().unwrap().send(msg).ok().unwrap())
            .await
            .unwrap();

        assert_eq!(qos, QoS::AtMostOnce);
        assert_eq!(client.get_subscription_status(topic), SubscriptionStatus::Live);

        client.publish_qos_0(topic, b"test1", false).await.unwrap();

        let msg = os_rx.await.unwrap();
        assert_eq!(msg.payload(), b"test1");

        sub.remove_callback().await;
        client.publish_qos_0(topic, b"test2", false).await.unwrap(); //panic! will occur if callback is called again
        
        time::sleep(Duration::from_secs(5)).await;
        drop(client);
    
        let disconnect_result = shutdown_handle.disconnect().await.unwrap();
        assert_eq!(disconnect_result.disconnect_sent, true);
        assert_eq!(disconnect_result.clean, true);
    }

    async fn broker_fn(mut broker: MiniBroker) -> io::Result<()>
    {
        broker.expect().connect().await?;
        broker.send(&ConnAckPacket(Ok(false))).await?;

        let sub = broker.expect().subscribe().await?;
        let sub_qos = sub.topics[0].1;
        broker.send(&SubAckPacket::new(sub.packet_id, &[Ok(sub_qos)])).await?;
    
        let msg1 = broker.recv_message().await?;
        let msg2 = msg1.to_outgoing(sub_qos, false, 1);
        broker.send_message(&msg2).await?;
    
        let msg3 = broker.recv_message().await?;
        let msg4 = msg3.to_outgoing(sub_qos, false, 2);
        broker.send_message(&msg4).await?;

        broker.expect_disconnect().await;
        Ok(())
    }
}

def_tests! {
    /// Simple test validating that publish packets with QoS 1 are re-sent if no `PUBACK`
    /// is received within a reasonable amount of time.
    check_publish_resend:
    
    async fn client_fn(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
    {
        debug!("==> Connected! session_present={}", client.was_session_present());
        client.publish_qos_1(topic, b"test", false, true).await.unwrap();
        drop(client);
    
        let disconnect_result = shutdown_handle.disconnect().await.unwrap();
        assert_eq!(disconnect_result.disconnect_sent, true);
        assert_eq!(disconnect_result.clean, true);
    }

    async fn broker_fn(mut broker: MiniBroker) -> io::Result<()>
    {
        broker.expect().connect().await?;
        broker.send(&ConnAckPacket(Ok(false))).await?;
        broker.expect().publish().await?;

        //In theory, the message will be re-sent because we don't reply in time
        broker.recv_message().await?;
        broker.expect_disconnect().await;
        Ok(())
    }
}

def_tests! {
    /// Ensure subscriptions to a topic pattern containing wildcards ('*' or '#') work
    test_wildcard_sub:
    
    async fn client_fn(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
    {
        let mut sub_a = client.subscribe_unbounded(&format!("{}/*", topic), QoS::ExactlyOnce).await.unwrap().0;
        let mut sub_b1 = client.subscribe_unbounded(&format!("{}/a/*", topic), QoS::ExactlyOnce).await.unwrap().0;
        let mut sub_b2 = client.subscribe_unbounded(&format!("{}/*/b", topic), QoS::ExactlyOnce).await.unwrap().0;
        let mut sub_all = client.subscribe_unbounded(&format!("{}/#", topic), QoS::ExactlyOnce).await.unwrap().0;

        let topic_a = format!("{}/a", topic);
        let topic_b = format!("{}/a/b", topic);

        client.publish_qos_2(&topic_a, b"a", false, PublishEvent::None).await.unwrap();

        for sub in [&mut sub_a, &mut sub_all] {
            let msg = sub.recv().await.unwrap();
            debug!("[A] Received {:?} in topic {}", msg.payload(), msg.topic());

            assert_eq!(msg.topic(), topic_a);
            assert_eq!(msg.payload(), b"a");
        }

        client.publish_qos_2(&topic_b, b"b", false, PublishEvent::None).await.unwrap();

        for sub in [&mut sub_b1, &mut sub_b2, &mut sub_all] {
            let msg = sub.recv().await.unwrap();
            debug!("[B] Received {:?} in topic {}", msg.payload(), msg.topic());

            assert_eq!(msg.topic(), topic_b);
            assert_eq!(msg.payload(), b"b");
        }

        //Wait 1 second just in case, then check that all queues are empty
        time::sleep(Duration::from_secs(1)).await;

        for sub in [&mut sub_a, &mut sub_b1, &mut sub_b2, &mut sub_all] {
            assert!(matches!(sub.try_recv(), Err(TryRecvError::Empty)));
        }

        drop(client);
    
        let disconnect_result = shutdown_handle.disconnect().await.unwrap();
        assert_eq!(disconnect_result.disconnect_sent, true);
        assert_eq!(disconnect_result.clean, true);
    }

    async fn broker_fn(mut broker: MiniBroker) -> io::Result<()>
    {
        broker.expect().connect().await?;
        broker.send(&ConnAckPacket(Ok(false))).await?;
        
        for _ in 0..4 {
            let sub = broker.expect().subscribe().await?;
            let sub_qos = sub.topics[0].1;

            broker.send(&SubAckPacket::new(sub.packet_id, &[Ok(sub_qos)])).await?;
        }

        for pkt_id in 5..7 {
            let msg_in = broker.recv_message().await?;
            let msg_out = msg_in.to_outgoing(QoS::ExactlyOnce, false, pkt_id);

            broker.send_message(&msg_out).await?;
        }

        broker.expect_disconnect().await;
        Ok(())
    }
}

def_tests! {
    /// High-level test validating lmc's basic functionalities.
    complete:
    
    async fn client_fn(client: Client, shutdown_handle: ClientShutdownHandle, topic: &str)
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
        client.unsubscribe(topic.to_string()).await;
    
        time::sleep(Duration::from_secs(1)).await;
        client.publish_qos_0(topic, b"byebye2", false).await.unwrap();
        drop(client);
    
        let disconnect_result = shutdown_handle.disconnect().await.unwrap();
        assert_eq!(disconnect_result.disconnect_sent, true);
        assert_eq!(disconnect_result.clean, true);
    }

    async fn broker_fn(mut broker: MiniBroker) -> io::Result<()>
    {
        broker.expect().connect().await?;
        broker.send(&ConnAckPacket(Ok(false))).await?;
    
        let sub = broker.expect().subscribe().await?;
        let sub_qos = sub.topics[0].1;
        broker.send(&SubAckPacket::new(sub.packet_id, &[Ok(sub_qos)])).await?;
    
        let msg1 = broker.recv_message().await?;
        let msg2 = msg1.to_outgoing(sub_qos, false, 1);
        broker.send_message(&msg2).await?;
    
        broker.expect().pingreq().await?;
        broker.send(&PingRespPacket).await?;
    
        let msg3 = broker.recv_message().await?;
        let msg4 = msg3.to_outgoing(sub_qos, false, 2);
        broker.send_message(&msg4).await?;
    
        let unsub = broker.expect().unsub().await?;
        broker.send(&UnsubAckPacket::new(unsub.packet_id)).await?;
    
        broker.recv_message().await?;
        broker.expect_disconnect().await;
        Ok(())
    }
}
