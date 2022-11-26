//! This crates provides a basic MQTT client implementation with a high-level, asynchronous interface.
//! 
//! # Connecting
//! 
//! Connecting to a broker can be achieved using the [`Client::connect()`] function, by passing a
//! hostname (or IP address) together with [`Options`]:
//! 
//! ```
//! # tokio_test::block_on(async {
//! use lmc::{Options, Client, QoS};
//! 
//! let mut opts = Options::new("client_id")
//!     .enable_tls()
//!     .expect("Failed to load native system TLS certificates");
//!     
//! opts.set_username("username")
//!     .set_password(b"password");
//! 
//! # return; //We can't really test this in doctests
//! let (client, shutdown_handle) = Client::connect("localhost", opts)
//!     .await
//!     .expect("Failed to connect to broker!");
//! 
//! let (subscription, sub_qos) = client.subscribe_unbounded("my_topic", QoS::AtLeastOnce)
//!     .await
//!     .expect("Failed to subscribe to 'my_topic'");
//! 
//! println!("Subscribed to topic with QoS {:?}", sub_qos);
//! 
//! client.publish_qos_1("my_topic", b"it works!", false, true)
//!     .await
//!     .expect("Failed to publish message in 'my_topic'");
//! 
//! let msg = subscription.recv().await.expect("Failed to await message");
//! println!("Received {}", msg.payload_as_utf8().unwrap());
//! 
//! shutdown_handle.disconnect().await.expect("Could not disconnect gracefully");
//! # });
//! ```
//! 
//! # Publishing messages
//! 
//! The basic ways to publish messages are:
//! - [`Client::publish_qos_0()`] to publish a message with a Quality of Service of "AtMostOnce". Unfortunately,
//!   there is no way to know if and when this message has reached the broker safely.
//! - [`Client::publish_qos_1()`] to publish a message with a Quality of Service of "AtLeastOnce". By setting
//!   `await_ack` to true, it is possible to wait for the acknowledgment packet and make sure that the broker
//!   has received the message.
//! - [`Client::publish_qos_2()`] to publish a message with a Quality of Service of "ExactlyOnce". By specifying
//!   a value other than [`PublishEvent::None`] to `await_event`, it is possible to wait for the acknowledgment
//!   packet and make sure that the broker has received the message.
//! 
//! If it is not necessary to wait for the publish packet to be sent, [`Client::publish_no_wait()`] can also be used.
//! Finally, [`Client::try_publish()`] also be used to attempt to publish a packet with no need for any `await`.
//! 
//! # Subscribing to topics
//! 
//! There are multiple ways to subscribe to topics, each of them have their own limitations:
//!  - [`Client::subscribe_lossy()`] will create a subscription based on a bounded queue. If the queue is full when
//!    the message is received, it will not be pushed onto that queue and may be lost.
//!  - [`Client::subscribe_unbounded()`] will create a subscription based on an unbounded queue, bypassing the
//!    limitations of a bounded subscription at a slightly increased performance cost.
//!  - [`Client::subscribe_fast_callback()`] will cause the passed function to be called every time a message is
//!    received. However, the function must be thread-safe and cannot block.
//! 
//! There is also [`Client::subscribe_hold()`], which cannot be used to read messages but can be used to prevent
//! the client from unsubscribing from a topic automatically.
//! 
//! Note that LMC subscriptions and MQTT subscriptions are not the same thing. LMC subscriptions are created using
//! any of the `subscribe` methods. MQTT subscriptions are created by the implementation, as a result of the creation
//! of an LMC subscription to a **new** topic. If an LMC subscription exists for a given topic, that means that an MQTT
//! subscription already exists and there is thus no need to create a new one. This does mean that **only the first
//! LMC subscription will receive the retained messages** of a particular topic (if there are any). So, if the first
//! subscription is a "hold" subscription, retained messages will be lost.
//! 
//! MQTT subscriptions will only be cancelled (unsubscribed) if there are no more valid LMC subscription for that topic,
//! or if [`Client::unsubscribe()`] is called directly. Note that automatic unsubscribe can only be triggered by removing
//! a fast callback subscription or by an incoming message in that topic. This is because the transceiver task does not
//! actively check if subscription queues are closed.

#![feature(new_uninit)]
#![feature(get_mut_unchecked)]

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::net::{lookup_host, TcpSocket};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinError};
use tokio::time;

#[cfg(feature = "tls")]
pub mod tls;

pub mod subs;
mod options;
mod transport;
mod transceiver;
mod futures;
mod errors;
mod shared;

#[cfg(test)]
#[cfg(feature = "tls")]
mod tests;

pub use errors::{ConnectError, PublishError, TryPublishError, SubscribeError, TimeoutKind, ServerConnectError};
pub use options::{Options, LastWill};
pub use transceiver::ShutdownStatus;
pub use transceiver::packets::{IncomingPublishPacket as Message, PublishFlags, PublishPacketInfo as MessageInfo};

use options::{ConnectionConfig, OptionsT};
use transceiver::{TransceiverBuildData, Transceiver};
use transceiver::commands::{Command, PublishCommand, SubscribeCommand, UnsubCommand, UnsubKind, SubscriptionKind, FastCallback};
use transceiver::packets::{ConnectPacket, Encode, OutgoingPublishPacket};
use futures::*;
use shared::*;

#[cfg(feature = "tls")]
pub use tls::OptionsWithTls;

/// The **maximum** Quality of Service used to transmit and receive messages.
/// 
/// If the value used to publish a message is different than the value used
/// to subscribe to its corresponding topic, the **effective** quality of
/// service will be the lowest of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum QoS
{
    /// Messages will be transmitted or received at most once.
    /// 
    /// This is the lowest (but fastest) quality of service,
    /// involving a single packet. However, it is possible that
    /// the message may never be transmitted or received.
    AtMostOnce = 0,

    /// Messages will be transmitted or received at least once.
    /// 
    /// This quality of service involves at least 2 packets.
    /// Using this value as the **effective** quality of service
    /// means that messages will definitely be received by the
    /// recipient(s), but they may receive multiple copies of it.
    AtLeastOnce = 1,

    /// Messages will be transmitted or received exactly once.
    /// 
    /// This is the highest (but slowest) quality of service,
    /// involving at least 4 packets. However, if used as the
    /// **effective** quality of service, it ensures that
    /// the packet **will** be received by recipient(s) without
    /// any duplicates.
    ExactlyOnce = 2
}

/// A clonable, thread-safe handle to an MQTT client that can be used to
/// publish messages, and subscribe/unsubscribe to/from topics.
/// 
/// It can also be used to disconnect the client from the broker, however
/// it is recommended to disconnect using the [`ClientShutdownHandle`]
/// linked to this client.
#[derive(Clone)]
pub struct Client
{
    was_session_present: bool,
    cmd_queue: mpsc::Sender<Command>,
    shared: Arc<ClientShared>
}

/// An owned handle that can be used to disconnect and shutdown an MQTT client's
/// transceiver task and obtain its [`ShutdownStatus`].
/// 
/// Should this value be dropped without calling [`ClientShutdownHandle::disconnect()`]
/// beforehand, the client's transceiver task will be _detached_.
pub struct ClientShutdownHandle
{
    cmd_queue: mpsc::Sender<Command>,
    join_handle: JoinHandle<ShutdownStatus>
}

/// Enumerates the different stages of publishing a message with [`QoS::ExactlyOnce`]
/// that can be awaited on.
#[derive(Debug, Clone, Copy)]
pub enum PublishEvent
{
    /// Submit the publish packet to the transceiver task and return, do not wait
    /// for completion. Shutting down the transceiver task after this may result
    /// in the message not being transmitted at all.
    None,

    /// Submit the publish packet to the transceiver task and await its corresponding
    /// `PUBREC` packet, indicated that the server has received the message.
    /// 
    /// Shutting down the transceiver task at this point may be safe.
    Received,

    /// Submit the publish packet to the transceiver task and await its corresponding
    /// `PUBCOMP` packet, marking the end the transmission of the message.
    /// 
    /// Shutting down the transceiver task after this is safe.
    Complete
}

/// Utility enumeration to store the future corresponding to a [`PublishEvent`].
enum PublishEventFuture
{
    None,
    Received(PublishFuture<RecNotifierMapAccessor>),
    Complete(PublishFuture<CompNotifierMapAccessor>)
}

/// Utility struct to ensure that a subscription reference is safely released
/// should the function it is used in return in a unexpected manner (error or
/// cancelled future).
struct ReleaseRefOnDrop<'a>
{
    release: bool,
    shared: &'a ClientShared,
    topic: &'a str
}

impl<'a> ReleaseRefOnDrop<'a>
{
    fn arm(shared: &'a ClientShared, topic: &'a str) -> Self
    {
        Self { release: true, shared, topic }
    }

    fn disarm(mut self)
    {
        self.release = false;
    }
}

impl<'a> Drop for ReleaseRefOnDrop<'a>
{
    fn drop(&mut self)
    {
        if self.release {
            match self.shared.subs.lock().get_mut(self.topic) {
                Some(SubscriptionState::Existing(data)) => data.ref_count -= 1,
                Some(SubscriptionState::Pending(data)) => data.ref_count -= 1,
                None => {}
            }
        }
    }
}

/// Utility enumeration specifying when to wait for a subscription future.
enum SubWait
{
    /// The subscription already exists and has the specified quality of service.
    /// No need to await the subscription's completion.
    DontWait(QoS),

    /// The subscription is pending. Wait for completion before sending the
    /// subscribe command to the transceiver task.
    Before,

    /// The subscription does not exist and has to be created. Send the subscribe
    /// command to the transceiver task right away and await completion after.
    After
}

impl Client
{
    /// Attempts to connect to the MQTT broker at the specified `host` using the specified [`Options`].
    /// 
    /// The `host` parameter should only contain a hostname or an IP address, optionally followed by a port
    /// number, separated by colons (e.g. `example.com` or `127.0.0.1:1234`). If the port is missing, the
    /// default port (specified in `options`) will be used to establish the connection.
    /// 
    /// If the connection succeeds and is accepted by the broker, two handles will be returned:
    ///  - A [`Client`] handle that can be used to publish messages and (un)susbcribe to/from topics
    ///  - A [`ClientShutdownHandle`] that can be used to disconnect the client's transceiver task
    /// 
    /// An example can be found in this module's documentation.
    pub async fn connect<'a, C, CC>(host: &str, options: OptionsT<'a, CC>) -> Result<(Client, ClientShutdownHandle), ConnectError>
    where CC: ConnectionConfig<C>
    {
        let (options, conn_cfg) = options.separate_connection_cfg();
        let tmp;

        let (host_and_port, port_pos) = match host.find(':') {
            Some(x) => (host, x),
            None => {
                tmp = format!("{}:{}", host, options.default_port());
                (tmp.as_str(), host.len())
            }
        };

        let host_only = &host_and_port[..port_pos];
        let conn = conn_cfg.create_connection(host_only)?;

        let addr = time::timeout(options.dns_timeout, lookup_host(host_and_port)).await
            .map_err(|_| ConnectError::Timeout(TimeoutKind::DnsLookup))?
            .map_err(ConnectError::LookupHostError)?
            .filter(|addr| options.enabled_ip_versions.supports(addr))
            .next()
            .ok_or(ConnectError::HostnameNotFound)?;

        let socket = (if addr.is_ipv6() { TcpSocket::new_v6() } else { TcpSocket::new_v4() }).map_err(ConnectError::IoError)?;

        let stream = time::timeout(options.tcp_connect_timeout, socket.connect(addr)).await
            .map_err(|_| ConnectError::Timeout(TimeoutKind::TcpConnect))?
            .map_err(ConnectError::IoError)?;

        if options.no_delay {
            stream.set_nodelay(true).map_err(ConnectError::IoError)?;
        }

        let (cmd_tx, cmd_rx) = mpsc::channel(1024);
        let (conn_tx, conn_rx) = oneshot::channel();
        let transport = CC::create_transport(stream, conn);

        let connect_packet = ConnectPacket {
            clean_session: options.clean_session,
            keep_alive: options.keep_alive,
            client_id: options.client_id,
            will: options.last_will,
            username: options.username,
            password: options.password
        }.make_arc_packet();

        let shared = Arc::new(ClientShared::new());

        let build_data = TransceiverBuildData {
            transport,
            cmd_queue: cmd_rx,
            connect_sig: conn_tx,
            connect_packet,
            ping_interval: Duration::from_secs(options.keep_alive as u64),
            shared: shared.clone(),
            packet_resend_delay: options.packets_resend_delay.max(Duration::from_secs(1))
        };

        let join_handle = Transceiver::spawn(build_data);
        let conn_result = time::timeout(options.mqtt_connect_timeout, conn_rx).await;

        let conn_result = match conn_result {
            Ok(Ok(Ok(x)))  => Ok(x),
            Ok(Ok(Err(x))) => Err(ConnectError::ServerError(x)),
            Ok(Err(_))     => Err(ConnectError::OneshotRecvError),
            Err(_) => {
                let _ = cmd_tx.send(Command::Disconnect).await;
                return Err(ConnectError::Timeout(TimeoutKind::MqttConnect));
            }
        };
        
        match conn_result {
            Ok(was_session_present) => {
                let shutdown_handle = ClientShutdownHandle {
                    cmd_queue: cmd_tx.clone(),
                    join_handle
                };

                let client = Client {
                    was_session_present,
                    cmd_queue: cmd_tx,
                    shared
                };

                Ok((client, shutdown_handle))
            },
            Err(err) => {
                drop(cmd_tx);
                let _ = join_handle.await;
                Err(err)
            }
        }
    }

    /// Returns the `session_present` flags returned by the broker when connecting.
    /// Indicates if a session corresponding to this client's ID was already existing and has been resumed.
    pub fn was_session_present(&self) -> bool
    {
        self.was_session_present
    }

    #[inline]
    fn make_publish_cmd(&self, topic: &str, payload: &[u8], qos: QoS, retain: bool) -> PublishCommand
    {
        let packet_id = if qos == QoS::AtMostOnce { 0 } else { self.shared.next_packet_id.fetch_add(1, Ordering::Relaxed) };

        let packet = OutgoingPublishPacket {
            flags: PublishFlags::new(false, qos, retain),
            topic, packet_id, payload
        };

        PublishCommand {
            packet_id,
            qos,
            packet: packet.make_arc_packet()
        }
    }

    /// Attempts to publish a message. This flavour is non-blocking and does not need to be awaited on, but
    /// can fail if the client's command queue is full.
    /// 
    /// There is no guarantee that the message will be transmitted to the broker as soon as this call returns,
    /// so the developer should wait a bit before shutting the client down. If this is not the expected
    /// behaviour, check the other `publish` methods.
    /// 
    /// For details regarding this method's parameters, check the MQTT protocol.
    pub fn try_publish(&self, topic: &str, payload: &[u8], qos: QoS, retain: bool) -> Result<(), TryPublishError>
    {
        use mpsc::error::*;
        let cmd = self.make_publish_cmd(topic, payload, qos, retain);
        
        match self.cmd_queue.try_send(cmd.into()) {
            Ok(x) => Ok(x),
            Err(TrySendError::Closed(_)) => Err(TryPublishError::TransceiverTaskTerminated),
            Err(TrySendError::Full(_)) => Err(TryPublishError::QueueFull)
        }
    }

    /// Waits for a free spot in the client's command queue and enqueues a publish command.
    /// 
    /// There is no guarantee that the message will be transmitted to the broker as soon as this call returns,
    /// so the developer should wait a bit before shutting the client down. If this is not the expected
    /// behaviour, check the other `publish` methods.
    /// 
    /// For details regarding this method's parameters, check the MQTT protocol.
    pub async fn publish_no_wait(&self, topic: &str, payload: &[u8], qos: QoS, retain: bool) -> Result<(), PublishError>
    {
        let cmd = self.make_publish_cmd(topic, payload, qos, retain);
        self.cmd_queue.send(cmd.into()).await.map_err(|_| PublishError::TransceiverTaskTerminated)
    }

    /// Waits for a free spot in the client's command queue and enqueues a publish command with the lowest
    /// possible Quality of Service ([`QoS::AtMostOnce`]). This is exactly the same as calling
    /// [`Self::publish_no_wait()`] with [`QoS::AtMostOnce`].
    /// 
    /// There is no guarantee that the message will be transmitted to the broker as soon as this call returns,
    /// so the developer should wait a bit before shutting the client down. Reception by the broker will also
    /// never be guaranteed for a message with [`QoS::AtMostOnce`]; it is thus recommended to use
    /// [`Self::publish_qos_1()`] or [`Self::publish_qos_2()`] if the developer needs to ensure that the message
    /// has been successfully received by the broker.
    /// 
    /// For details regarding this method's parameters, check the MQTT protocol.
    pub async fn publish_qos_0(&self, topic: &str, payload: &[u8], retain: bool) -> Result<(), PublishError>
    {
        let cmd = self.make_publish_cmd(topic, payload, QoS::AtMostOnce, retain);
        self.cmd_queue.send(cmd.into()).await.map_err(|_| PublishError::TransceiverTaskTerminated)
    }

    /// Waits for a free spot in the client's command queue, enqueues a publish command with [`QoS::AtLeastOnce`],
    /// and optionally waits for the corresponding `PUBACK` packet, indicating that the message was successfully
    /// received by the broker (based on the value of `await_ack`).
    /// 
    /// If `await_ack` is `false`, then this is exactly the same as calling [`Self::publish_no_wait()`] with
    /// [`QoS::AtLeastOnce`], and there will be no guarantee that the message will be transmitted to the broker
    /// as soon as this call returns.
    /// 
    /// Note that the broker is free to not relay the message to any clients if, for instance, this client does
    /// not have the permission to publish to the specified topic. On top of that, clients subscribed to the
    /// topic may have chosen a different Quality of Service. So, even if this method succeeds with `await_ack`
    /// set to true `true`, there is no guarantee that the message was received by any client.
    /// 
    /// For details regarding this method's parameters, check the MQTT protocol.
    pub async fn publish_qos_1(&self, topic: &str, payload: &[u8], retain: bool, await_ack: bool) -> Result<(), PublishError>
    {
        let cmd = self.make_publish_cmd(topic, payload, QoS::AtLeastOnce, retain);

        let opt_fut = match await_ack {
            true  => Some(PublishFuture::new(cmd.packet_id, AckNotifierMapAccessor(self.shared.clone()))),
            false => None
        };

        self.cmd_queue.send(cmd.into()).await.map_err(|_| PublishError::TransceiverTaskTerminated)?;

        match opt_fut {
            Some(x) => if x.await { Err(PublishError::TransceiverTaskTerminated) } else { Ok(()) },
            None => Ok(())
        }
    }

    /// Waits for a free spot in the client's command queue, enqueues a publish command with [`QoS::ExactlyOnce`],
    /// and optionally waits for (based on the value of `await_event`):
    ///  - The corresponding `PUBREC` packet, indicating that the message was successfully received by the broker
    ///  - The corresponding `PUBCOMP` packet, indicating the end of the QoS 2 message transmission 
    /// 
    /// If `await_event` is [`PublishEvent::None`], then this is exactly the same as calling [`Self::publish_no_wait()`]
    /// with [`QoS::ExactlyOnce`], and there will be no guarantee that the message will be transmitted to the broker
    /// as soon as this call returns.
    /// 
    /// Note that the broker is free to not relay the message to any clients if, for instance, this client does
    /// not have the permission to publish to the specified topic. On top of that, clients subscribed to the
    /// topic may have chosen a different Quality of Service. So, even if this method succeeds with `await_event`
    /// different from [`PublishEvent::None`], there is no guarantee that the message was received by any client.
    /// 
    /// For details regarding this method's parameters, check the MQTT protocol.
    pub async fn publish_qos_2(&self, topic: &str, payload: &[u8], retain: bool, await_event: PublishEvent) -> Result<(), PublishError>
    {
        let cmd = self.make_publish_cmd(topic, payload, QoS::ExactlyOnce, retain);

        let opt_fut = match await_event {
            PublishEvent::None     => PublishEventFuture::None,
            PublishEvent::Received => PublishEventFuture::Received(PublishFuture::new(cmd.packet_id, RecNotifierMapAccessor(self.shared.clone()))),
            PublishEvent::Complete => PublishEventFuture::Complete(PublishFuture::new(cmd.packet_id, CompNotifierMapAccessor(self.shared.clone())))
        };

        self.cmd_queue.send(cmd.into()).await.map_err(|_| PublishError::TransceiverTaskTerminated)?;

        let ttt = match opt_fut {
            PublishEventFuture::None        => false,
            PublishEventFuture::Received(x) => x.await,
            PublishEventFuture::Complete(x) => x.await
        };

        if ttt { Err(PublishError::TransceiverTaskTerminated) } else { Ok(()) }
    }

    async fn subscribe<R, F>(&self, topic: &str, qos_hint: QoS, func: F) -> Result<(R, QoS), SubscribeError>
    where F: FnOnce() -> (R, SubscriptionKind)
    {
        let mut sub_map = self.shared.subs.lock();

        let actual_qos = match sub_map.get_mut(topic) {
            Some(SubscriptionState::Existing(data)) => {
                //Add ref, then send command
                data.ref_count += 1;
                SubWait::DontWait(data.qos)
            },
            Some(SubscriptionState::Pending(data)) => {
                //Add ref, await result, and only then send command
                data.ref_count += 1;
                SubWait::Before
            },
            None => {
                //Create the map entry, then send command, then await result
                sub_map.insert(topic.into(), PendingSusbcription::with_one_ref().into());
                SubWait::After
            }
        };

        drop(sub_map);

        let release_ref_on_drop = ReleaseRefOnDrop::arm(&self.shared, topic);
        let actual_qos = match actual_qos {
            SubWait::DontWait(x) => Some(x),
            SubWait::Before      => Some(SubscribeFuture::new(&self.shared, topic).await.map_err(|_| SubscribeError::RefusedByBroker)?),
            SubWait::After       => None
        };

        let (ret, kind) = func();

        let cmd = SubscribeCommand {
            topic: topic.into(),
            qos: qos_hint,
            kind
        };

        //Note: we could skip this safely for hold subscriptions if the subscription already exists...
        self.cmd_queue.send(cmd.into()).await.map_err(|_| SubscribeError::TransceiverTaskTerminated)?;

        //At this point, the subscribe command has been successfully sent to the transceiver task.
        //It will take care of releasing the reference, so we don't need to that anymore.
        release_ref_on_drop.disarm();

        let actual_qos = match actual_qos {
            Some(x) => x,
            None    => SubscribeFuture::new(&self.shared, topic).await.map_err(|_| SubscribeError::RefusedByBroker)?
        };

        Ok((ret, actual_qos))
    }

    /// Creates a [`subs::Hold`] subscription to the specified topic. Hold subscriptions cannot be used to read
    /// messages, they are only here to prevent the client from unsubscribing from a topic automatically. If you
    /// wish to read messages, use any other `subscribe` methods.
    /// 
    /// If the client is already subscribed to the topic, then this function will only wait for a free spot on the
    /// command queue to submit the subscribe command, and will completely ignore `qos_hint`. Otherwise, it will
    /// send a `SUBSCRIBE` packet to the broker with the specified `qos_hint` and wait for a response. Because the
    /// returned "hold" subscription will be the sole subscription for this topic, all incoming messages (including
    /// the retained messages, if any) will be dropped, until a new subscription is created using any of the other
    /// `subscribe` methods.
    /// 
    /// The returned value contains the subscription object (which can be used to cancel the subscription),
    /// as well as the actual [`QoS`] of this subscription as specified by the broker.
    pub async fn subscribe_hold(&self, topic: String, qos_hint: QoS) -> Result<(subs::Hold, QoS), SubscribeError>
    {
        let (_, qos) = self.subscribe(&topic, qos_hint, move || ((), SubscriptionKind::AddRefOnly)).await?;
        let ret = subs::Hold::new(self.shared.clone(), topic);

        Ok((ret, qos))
    }

    /// Creates a "lossy" subscription to the specified topic, with the specified capacity. "Lossy" subscriptions
    /// use a bounded MPSC channel to read incoming messages. Because the transceiver task cannot afford to wait,
    /// messages on the queue are sent using [`mpsc::Sender::try_send()`] and are dropped if the queue is full,
    /// meaning that messages can be lost if they are not dequeued quickly enough or if the queue capacity
    /// is too low. If this behaviour is not wanted, use [`Self::subscribe_unbounded()`] which relies on a
    /// [`mpsc::UnboundedSender`] instead, for a slightly higher cost in performance.
    /// 
    /// If the client is already subscribed to the topic, then this function will only wait for a free spot on the
    /// command queue to submit the subscribe command, and will completely ignore `qos_hint`. Otherwise, it will
    /// send a `SUBSCRIBE` packet to the broker with the specified `qos_hint` and wait for a response. In that case,
    /// retained packets (if there are any) are guarateed to be pushed onto the returned queue, unless the queue
    /// is full.
    /// 
    /// The returned value contains the [`mpsc::Receiver`] end of the queue as well as the actual [`QoS`] of this
    /// subscription as specified by the broker. If the receiving end of the queue is closed (by dropping it for
    /// instance), then the client may unsubscribe from the topic.
    pub fn subscribe_lossy<'a>(&'a self, topic: &'a str, qos_hint: QoS, queue_cap: usize) -> impl Future<Output = Result<(mpsc::Receiver<Message>, QoS), SubscribeError>> + 'a
    {
        self.subscribe(topic, qos_hint, move || {
            let (tx, rx) = mpsc::channel(queue_cap);
            (rx, SubscriptionKind::Lossy(tx))
        })
    }

    /// Creates an "unbounded" subscription to the specified topic. "Unbounded" subscriptions use an unbounded
    /// channel to read incoming messages. This is the safest way to receive messages, but may come at a slighly
    /// increased performance cost. If performance is an issue, use [`Self::subscribe_lossy()`].
    /// 
    /// If the client is already subscribed to the topic, then this function will only wait for a free spot on the
    /// command queue to submit the subscribe command, and will completely ignore `qos_hint`. Otherwise, it will
    /// send a `SUBSCRIBE` packet to the broker with the specified `qos_hint` and wait for a response. In that case,
    /// retained packets (if there are any) are guarateed to be pushed onto the returned queue.
    /// 
    /// The returned value contains the [`mpsc::UnboundedReceiver`] end of the queue as well as the actual [`QoS`]
    /// of this subscription as specified by the broker. If the receiving end of the queue is closed (by dropping it
    /// for instance), then the client may unsubscribe from the topic.
    pub fn subscribe_unbounded<'a>(&'a self, topic: &'a str, qos_hint: QoS) -> impl Future<Output = Result<(mpsc::UnboundedReceiver<Message>, QoS), SubscribeError>> + 'a
    {
        self.subscribe(topic, qos_hint, move || {
            let (tx, rx) = mpsc::unbounded_channel();
            (rx, SubscriptionKind::Unbounded(tx))
        })
    }

    /// Creates a [`subs::Callback`] subscription to the specified topic. Callback subscriptions simply call
    /// the specified function whenever a message is received. This is the lightest form of subscription, however
    /// it does come with some constraints:
    ///  - `callback` must be thread-safe (implement [`Send`] and [`Sync`])
    ///  - `callback` must never block
    ///
    /// If the client is already subscribed to the topic, then this function will only wait for a free spot on the
    /// command queue to submit the subscribe command, and will completely ignore `qos_hint`. Otherwise, it will
    /// send a `SUBSCRIBE` packet to the broker with the specified `qos_hint` and wait for a response. In that case,
    /// `callback` is guaranteed to be called for each retained messages (should there be any).
    /// 
    /// The returned value contains the subscription object (which can be used to cancel the subscription),
    /// as well as the actual [`QoS`] of this subscription as specified by the broker.
    pub async fn subscribe_fast_callback<C>(&self, topic: String, qos_hint: QoS, callback: C) -> Result<(subs::Callback, QoS), SubscribeError>
    where C: FnMut(Message) + Send + Sync + 'static
    {
        let (id, qos) = self.subscribe(&topic, qos_hint, move || {
            let id = self.shared.next_callback_id.fetch_add(1, Ordering::Relaxed);
            (id, SubscriptionKind::FastCallback(FastCallback { id, f: Box::new(callback) }))
        }).await?;

        let ret = subs::Callback::new(self.cmd_queue.clone(), topic, id);
        Ok((ret, qos))
    }

    /// Unsubscribes from the specified topic, regardless of any existing subscription to that topic. If there
    /// are valid MPSC channels for that topic, they will be closed. If the client is not subscribed to this
    /// topic, this call will have no effect.
    pub async fn unsubscribe(&self, topic: String)
    {
        let cmd = UnsubCommand { topic, kind: UnsubKind::Immediate };
        let _ = self.cmd_queue.send(cmd.into()).await;
    }

    /// Returns `true` if the transceiver task is still running, meaning the MQTT client is probably still
    /// connected.
    pub fn is_transceiver_task_running(&self) -> bool
    {
        !self.cmd_queue.is_closed()
    }

    /// Returns `true` if the transceiver task stopped, which may occur if [`Client::disconnect()`] or
    /// [`ClientShutdownHandle::disconnect()`] are closed, but also if the connection is closed
    /// unexpectedly.
    pub fn did_transceiver_task_stop(&self) -> bool
    {
        self.cmd_queue.is_closed()
    }

    /// Enqueues a disconnect command, causing the transceiver task to attempt to disconnect gracefully
    /// and shutdown. This will take some time to complete and the task might still be alive when this
    /// function returns. [`Self::is_transceiver_task_running()`] or [`Self::did_transceiver_task_stop()`]
    /// can be used to check if the task is still running. Ideally, [`ClientShutdownHandle::disconnect()`]
    /// should be used instead as it waits for the task's completion.
    /// 
    /// Pending publish packets may not be sent, and existing subscriptions will be closed.
    pub async fn disconnect(self)
    {
        let _ = self.cmd_queue.send(Command::Disconnect).await;
    }
}

impl ClientShutdownHandle
{
    /// Enqueues a disconnect command, causing the transceiver task to attempt to disconnect gracefully
    /// and shutdown. Unlike [`Client::disconnect()`], this function will wait for the transceiver task
    /// to complete before returning, and also returns a [`ShutdownStatus`], which can be used to know
    /// whether the client managed to disconnect gracefully or not.
    /// 
    /// Pending publish packets may not be sent, and existing subscriptions will be closed.
    pub async fn disconnect(self) -> Result<ShutdownStatus, JoinError>
    {
        let _ = self.cmd_queue.send(Command::Disconnect).await;
        self.join_handle.await
    }
}
