use std::time::{Instant, Duration};
use std::boxed::Box;
use std::io;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::Waker;

use tokio::sync::{mpsc, oneshot};
use tokio::{time, select};
use tokio::task::{self, JoinHandle};
use fxhash::FxHashMap;
use log::{error, warn, debug};

pub mod byte_io;
mod packet_reader;
pub mod util;
pub mod subscription;
pub mod commands;
pub mod packets;

use super::{QoS, ClientShared, SubscriptionState, ExistingSubscription, NotifyResult, NotifierMap};
use super::transport::Transport;
use super::errors::ServerConnectError;

use util::{IdType, panic_in_test};
use commands::{Command, SubscriptionKind, UnsubKind};
use subscription::Subscription;
use packet_reader::{PacketReader, PacketReadState};

use packets::{IncomingPacket, IncomingPublishPacket, PublishFlags,
    PingReqPacket, Encode, DisconnectPacket, PubAckPacket, PubRecPacket, PubRelPacket,
    UnsubscribePacket, PacketType, PubCompPacket, SubscribePacket};

/// Packet in the process of being transmitted
struct TxPacket
{
    bytes: Arc<[u8]>,

    /// Number of bytes already transmitted
    pos: usize,

    /// If this is true, then the first byte of the packet should be altered
    /// to include the `DUP` flag.
    set_dup: bool
}

/// Associates a packet ID to its transmission timestamp. This is used to
/// re-send packets if no reply is received within a reasonable delay.
#[derive(Clone, Copy)]
struct Timeout
{
    t: Instant,
    packet_id: IdType
}

impl Timeout
{
    fn now(packet_id: IdType) -> Self
    {
        Self {
            t: Instant::now(),
            packet_id
        }
    }

    fn refresh(self) -> Self
    {
        Self {
            t: Instant::now(),
            ..self
        }
    }
}

/// A queued outgoing packet
struct InternalPacket
{
    bytes: Arc<[u8]>,

    /// If this is true, then the first byte of the packet should be altered
    /// to include the `DUP` flag when being transmitted.
    set_dup: bool
}

/// A pending subscription awaiting a response from the server before.
/// This is used to re-transmit the `SUBSCRIBE` packet if no reply is
/// received in a reasonable time frame.
/// If the subscription succeeds, the LMC subscription stored in
/// [`PendingSub::kind`] is created.
struct PendingSub
{
    topic: String,
    kind: SubscriptionKind,
    packet: Arc<[u8]>
}

/// Contains the state of the transceiver task
pub(super) struct Transceiver
{
    transport: Box<dyn Transport>,
    cmd_queue: mpsc::Receiver<Command>,

    /// A map used to store packets that may be re-transmitted. This one excludes
    /// `SUBSCRIBE` packet as they require additional metadata and are thus stored
    /// in their own map.
    pending_packets: FxHashMap<IdType, Arc<[u8]>>,

    /// Pending `SUBSCRIBE` packets that may be re-transmitted.
    pending_subs: FxHashMap<u16, PendingSub>,

    /// Packet currently being transmitted, if any. When the transceiver task is
    /// created, this field will contain the `CONNECT` packet.
    tx_pkt: Option<TxPacket>,
    rx_pkt: PacketReader,
    alarm: time::Interval,

    /// A [`oneshot`] sender used to relay the success or failure of the connection.
    /// Then the task is created, it starts with a value. Once the connection is
    /// established, it will be [`None`].
    connect_sig: Option<oneshot::Sender<Result<bool, ServerConnectError>>>,

    /// Ping interval, as defined by the developer in the options.
    ping_interval: Duration,

    /// Last time a packet was transmitted. This is used to send `PINGREQ`
    /// packets.
    last_tx: Instant,

    /// A queue used to keep track of which packets have timed-out and need to be
    /// resent. The front correspond to the oldest packet that needs to be checked.
    timeout_queue: VecDeque<Timeout>,

    /// The outgoing packet queue
    internal_pkt_queue: VecDeque<InternalPacket>,
    shared: Arc<ClientShared>,
    subscriptions: FxHashMap<String, Subscription>,
    disconnect_sent: bool,
    cmd_queue_closed: bool,

    /// A cached `PINGREQ` packet. It's always the same, so no need to re-allocate
    /// it every time.
    ping_req: Arc<[u8]>,

    /// The amount of time to wait before re-sending a packet, should the broker
    /// not answer beforehand. This value is used to pop the elements of
    /// [`Self::timeout_queue`], and is set by the developer through the options.
    packet_resend_delay: Duration
}

/// Parameters used to create the transceiver task, wrapped in a structure
/// because they are too many!
pub(super) struct TransceiverBuildData
{
    pub transport: Box<dyn Transport>,
    pub cmd_queue: mpsc::Receiver<Command>,
    pub connect_sig: oneshot::Sender<Result<bool, ServerConnectError>>,
    pub connect_packet: Arc<[u8]>,
    pub ping_interval: Duration,
    pub shared: Arc<ClientShared>,
    pub packet_resend_delay: Duration
}

/// Describes how a subscribtion's ref count is handled when unsubscribing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RefCountCheck
{
    /// The subscription will only be cancelled if the ref count is zero.
    Check,

    /// The ref count will not be checked and the subscription will be
    /// cancelled immediately.
    Bypass,

    /// The ref count will first be decremented. If the resulting value is
    /// zero, then the subscription will be cancelled.
    Release
}

/// Describes the state in which the client's transceiver task finished.
#[derive(Debug, Clone, Copy)]
pub struct ShutdownStatus
{
    /// If this value is true, then the `DISCONNECT` packet has successfully
    /// been sent, and it's very unlikely that the last will message has been
    /// sent.
    pub disconnect_sent: bool,

    /// If this value is true, everything is proceeding as I have forseen and
    /// the connection has been closed cleanly.
    pub clean: bool
}

/// Tells the main loop what to do
enum TaskState
{
    WantContinue,
    WantStop,
    ConnectionClosed
}

impl Transceiver
{
    pub fn spawn(data: TransceiverBuildData) -> JoinHandle<ShutdownStatus>
    {
        task::spawn(async move {
            let mut t = Self {
                transport: data.transport,
                cmd_queue: data.cmd_queue,
                pending_packets: Default::default(),
                pending_subs: Default::default(),
                tx_pkt: Some(TxPacket { bytes: data.connect_packet.into(), pos: 0, set_dup: false }),
                rx_pkt: Default::default(),
                alarm: time::interval(Duration::from_secs(1)),
                connect_sig: Some(data.connect_sig),
                ping_interval: data.ping_interval,
                last_tx: Instant::now(),
                timeout_queue: VecDeque::with_capacity(1024),
                internal_pkt_queue: VecDeque::with_capacity(16),
                shared: data.shared,
                subscriptions: Default::default(),
                disconnect_sent: false,
                cmd_queue_closed: false,
                ping_req: PingReqPacket.make_arc_packet(),
                packet_resend_delay: data.packet_resend_delay
            };
    
            t.task().await
        })
    }

    fn connected(&self) -> bool
    {
        self.connect_sig.is_none()
    }

    /// Dispatches a message to all the LMC subscriptions associated to the message's
    /// topic.
    fn dispatch_message(&mut self, msg: IncomingPublishPacket) -> io::Result<()>
    {
        let topic = msg.topic();
        let sub = match self.subscriptions.get_mut(topic) {
            Some(x) => x,
            None => return Ok(())
        };

        sub.dispatch(&msg);

        if sub.can_unsub() {
            //Need to unsubscribe!
            self.unsub_immediate(topic, RefCountCheck::Check)
        } else {
            //Dispatched to at least one recipient; don't unsub
            Ok(())
        }
    }

    /// Attempts to transmit the specified packet using [`Self::queue_internal_packet()`], but also
    /// stores for re-transmission (should the server take too long to acknowledge that packet).
    /// 
    /// This should not be used for `SUBSCRIBE` packets as they have their dedicated map.
    fn queue_internal_packet_with_timeout(&mut self, pkt: Arc<[u8]>, id: IdType) -> io::Result<()>
    {
        if self.pending_packets.insert(id, pkt.clone()).is_none() {
            self.timeout_queue.push_back(Timeout::now(id));
        } else {
            panic_in_test!("Transceiver::queue_internal_packet_with_timeout should only be used with new packets. Waste of mallocs! Please report this to the crate developer.");
        }

        self.queue_internal_packet(pkt, false)
    }

    /// Removes a packet from the [`Self::pending_packet`] map, effectively marking it as received by
    /// the broker.
    /// 
    /// This should not be used for `SUBSCRIBE` packets as they have their dedicated map.
    fn clear_packet_timeout(&mut self, packet_id: IdType, info: &'static str) -> bool
    {
        if self.pending_packets.remove(&packet_id).is_some() {
            debug!("Received {}; cleared timeout for {:?} packet with ID {}", info, packet_id.ty, packet_id.id);
            true
        } else {
            false
        }
    }

    /// Performs the ref count check specified by `ref_cnt` (see [`RefCountCheck`]) and,
    /// if the check succeeds, deletes the subscription and sends an `UNSUB` packet.
    fn unsub_immediate(&mut self, topic: &str, ref_cnt: RefCountCheck) -> io::Result<()>
    {
        //Start by removing the entry in `ClientShared::subs`, provided all the refs are gone
        let mut subs = self.shared.subs.lock();

        if ref_cnt != RefCountCheck::Bypass {
            match subs.get_mut(topic) {
                Some(SubscriptionState::Existing(data)) => {
                    if ref_cnt == RefCountCheck::Release {
                        data.ref_count -= 1;
                    }

                    if data.ref_count > 0 {
                        return Ok(()); //Can't unsub right now
                    }
                },
                Some(SubscriptionState::Pending(data)) => {
                    if ref_cnt == RefCountCheck::Release {
                        data.ref_count -= 1;
                    }

                    if data.ref_count > 0 {
                        return Ok(()); //Can't unsub right now
                    }
                },
                None => {}
            }
        }

        subs.remove(topic);
        drop(subs);

        //Continue by removing the entry in `self.subscriptions`
        self.subscriptions.remove(topic);

        //Finish by sending the `Unsub` packet
        let packet_id = self.shared.next_packet_id.fetch_add(1, Ordering::Relaxed);
        let pkt = UnsubscribePacket { packet_id, topics: &[topic] }.make_arc_packet();

        self.queue_internal_packet_with_timeout(pkt, IdType::unsubscribe(packet_id))
    }

    /// Handles an incoming packet
    fn process_packet(&mut self, pkt: IncomingPacket) -> io::Result<bool>
    {
        if let Some(connect_sig) = self.connect_sig.take() {
            let conn_result = match pkt {
                IncomingPacket::ConnAck(x) => x.0,
                x => {
                    error!("Received {:?} packet before ConnAck; assuming wrong protocol", x.packet_type());
                    Err(ServerConnectError::ProtocolError)
                }
            };

            let _ = connect_sig.send(conn_result);
            return Ok(conn_result.is_err()); //Stop task if error
        }

        match pkt {
            IncomingPacket::ConnAck(_) => warn!("Received multiple 'ConnAck'; ignoring..."),
            IncomingPacket::SubAck(ack) => {
                if let Some(pending_sub) = self.pending_subs.remove(&ack.packet_id) {
                    if ack.sub_results().len() < 1 {
                        error!("Got SubAck with no results when trying to subscribe to topic \"{}\"", pending_sub.topic);
                    } else {
                        if ack.sub_results().len() > 1 {
                            warn!("Got SubAck with {} results when subscribing to topic \"{}\"; ignoring the others...", ack.sub_results().len(), pending_sub.topic);
                        }

                        let del_refs = match pending_sub.kind {
                            SubscriptionKind::AddRefOnly => 0,
                            _ => 1
                        };

                        let opt_wakers = if let Ok(qos) = ack.sub_results()[0] {
                            let mut subs = self.shared.subs.lock();
                            
                            match subs.get_mut(&pending_sub.topic) {
                                Some(x) => match x {
                                    SubscriptionState::Existing(data) => {
                                        panic_in_test!("Got SubAck for topic \"{}\" which already exists!", pending_sub.topic);
                                        data.ref_count -= del_refs;
                                        data.qos = qos;
                                        None
                                    },
                                    SubscriptionState::Pending(data) => {
                                        let ref_count = data.ref_count - del_refs;

                                        let pending = std::mem::replace(x, SubscriptionState::Existing(ExistingSubscription {
                                            ref_count,
                                            qos
                                        }));
    
                                        match pending {
                                            SubscriptionState::Pending(data) => Some(data.wakers),
                                            _ => panic!() //Compiler should see that this is impossible -- hopefully?
                                        }
                                    }
                                },
                                None => {
                                    panic_in_test!("Got SubAck for topic \"{}\" which is absent from the sub map! Something went wrong!", pending_sub.topic);

                                    subs.insert(pending_sub.topic.clone(), SubscriptionState::Existing(ExistingSubscription {
                                        ref_count: 1 - del_refs,
                                        qos
                                    }));

                                    None
                                }
                            }
                        } else {
                            match self.shared.subs.lock().remove(&pending_sub.topic) {
                                Some(SubscriptionState::Existing(_)) => {
                                    panic_in_test!("Got SubAck for topic \"{}\", which already exists, and this time the subscription failed!", pending_sub.topic);
                                    None
                                },
                                Some(SubscriptionState::Pending(data)) => Some(data.wakers),
                                None => {
                                    panic_in_test!("Got (failed) SubAck for topic \"{}\" which is absent from the sub map! Something went wrong!", pending_sub.topic);
                                    None
                                }
                            }
                        };

                        if let Some(wakers) = opt_wakers {
                            for opt_waker in wakers {
                                if let Some(waker) = opt_waker {
                                    waker.wake();
                                }
                            }
                        }
                    }

                    self.subscriptions.entry(pending_sub.topic).or_default().add(pending_sub.kind);
                }
            },
            IncomingPacket::Publish(msg) => {
                let info = msg.info();
                debug!("Received Publish with QoS {:?}", info.flags.qos());

                match info.flags.qos() {
                    QoS::AtMostOnce => self.dispatch_message(msg)?,
                    QoS::AtLeastOnce => {
                        //Reply with PubAck
                        let pkt = PubAckPacket::new(info.packet_id).make_arc_packet();
                        self.queue_internal_packet(pkt, false)?;
                        self.dispatch_message(msg)?;
                    },
                    QoS::ExactlyOnce => {
                        //Reply with PubRec
                        let pub_rec_id = IdType::pub_rec(info.packet_id);

                        if let Some(existing) = self.pending_packets.get(&pub_rec_id).cloned() {
                            //If it exists already, that means we've already received this message!
                            //Re-send the existing PubRec packet. Timeout should already be there.
                            //Obviously, don't re-dispatch that message

                            self.queue_internal_packet(existing, false)?;
                        } else {
                            //If it doesn't, create it, send it, store it, and create a timeout
                            let pkt = PubRecPacket::new(info.packet_id).make_arc_packet();
                            self.queue_internal_packet_with_timeout(pkt, pub_rec_id)?;
                            self.dispatch_message(msg)?;
                        }
                    }
                }
            },
            IncomingPacket::PubAck(ack) => {
                if self.clear_packet_timeout(IdType::publish(ack.packet_id), "PubAck") {
                    if let Some(NotifyResult::WithWaker(waker)) = self.shared.notify_ack.lock().remove(&ack.packet_id) {
                        waker.wake();
                    }
                }
            },
            IncomingPacket::PubRec(rec) => {
                //First, reply with `PubRel`
                let pub_rel_id = IdType::pub_rel(rec.packet_id);

                if let Some(existing) = self.pending_packets.get(&pub_rel_id).cloned() {
                    //If it exists already, just send it back.
                    //No need to create a new timeout as there should already be one somewhere (though it might fire too early but that's okay)

                    self.queue_internal_packet(existing, false)?;
                } else {
                    //If it doesn't, create it, send it, store it, and create a timeout
                    let pkt = PubRelPacket::new(rec.packet_id).make_arc_packet();
                    self.queue_internal_packet_with_timeout(pkt, pub_rel_id)?;
                }

                //Then, if there was a backed-up `Publish` packet, release it
                if self.clear_packet_timeout(IdType::publish(rec.packet_id), "PubRec") {
                    //Finally, if something was awaiting this `PubRel`, wake it up
                    if let Some(NotifyResult::WithWaker(waker)) = self.shared.notify_rec.lock().remove(&rec.packet_id) {
                        waker.wake();
                    }
                }
            },
            IncomingPacket::PubRel(rel) => {
                self.clear_packet_timeout(IdType::pub_rec(rel.packet_id), "PubRel");

                //Always reply with PubComp
                let pkt = PubCompPacket::new(rel.packet_id).make_arc_packet();
                self.queue_internal_packet(pkt, false)?;
            },
            IncomingPacket::PubComp(comp) => {
                if self.clear_packet_timeout(IdType::pub_rel(comp.packet_id), "PubComp") {
                    if let Some(NotifyResult::WithWaker(waker)) = self.shared.notify_comp.lock().remove(&comp.packet_id) {
                        waker.wake();
                    }
                }
            },
            IncomingPacket::UnsubAck(ack) => drop(self.clear_packet_timeout(IdType::unsubscribe(ack.packet_id), "UnsubAck")),
            IncomingPacket::PingResp(_) => debug!("Pong!")
        }

        Ok(false)
    }

    /// Tries to send a packet immediately. If the operation is incomplete, sets is as the current
    /// value of [`Self::tx_pkt`] so that it can be transmitted later.
    /// 
    /// This function **SHOULD NOT** be called if a packet is already being transmitted (e.g., if
    /// [`Self::tx_pkt`] is not [`None`]) as it would break stuff.
    /// 
    /// If `set_dup` is true, then the first byte of the packet will be altered
    /// to include the `DUP` flag.
    fn try_send_packet(&mut self, bytes: Arc<[u8]>, set_dup: bool) -> io::Result<()>
    {
        debug_assert!(self.tx_pkt.is_none(), "Transceiver::try_send_packet() shouldn't be called if a packet is already in the process of being transmitted");

        let mut written;

        if set_dup {
            written = self.transport.write(&[bytes[0] | PublishFlags::DUP], true)?;

            if written > 0 {
                written += self.transport.write(&bytes[written..], true)?;
            }
        } else {
            written = self.transport.write(&bytes, true)?;
        }
    
        if written >= bytes.len() {
            //We managed to write the entire thing in one go!
            self.transport.flush()?;
        } else {
            self.tx_pkt = Some(TxPacket { bytes, pos: written, set_dup });
        }

        Ok(())
    }

    /// Tries to send the specified packet using [`Self::try_send_packet()`] if no packet
    /// is currently in the process of being transmitted. Otherwise, pushes the packet onto
    /// the internal packet queue so that it can be transmitted later.
    fn queue_internal_packet(&mut self, bytes: Arc<[u8]>, set_dup: bool) -> io::Result<()>
    {
        if self.tx_pkt.is_none() {
            //After `try_send_packet`:
            // - Either `self.tx_pkt` is None, meaning the queue *SHOULD* be empty
            // - Or `self.tx_pkt` is Some, meaning we can't poll the queue anyway

            self.try_send_packet(bytes, set_dup)
        } else {
            self.internal_pkt_queue.push_back(InternalPacket { bytes, set_dup });
            Ok(())
        }
    }

    /// Marks the packet currently being transmitted as fully transmitted, and pops the
    /// next packet to be transmitted from the internal packet queue (provided its not
    /// empty).
    fn clear_tx_packet(&mut self) -> io::Result<()>
    {
        self.tx_pkt = None;

        while let Some(new_pkt) = self.internal_pkt_queue.pop_front() {
            self.try_send_packet(new_pkt.bytes, new_pkt.set_dup)?;

            if self.tx_pkt.is_some() {
                break;
            }
        }

        Ok(())
    }

    /// Attempts to transmit all queued packets. Will return `Err(WouldBlock)` if we need to try
    /// again later. Returns `Ok(true)` if the connection was closed, and `Ok(false)` if there is
    /// nothing left to transmit.
    fn continue_writing_packet(&mut self) -> io::Result<bool>
    {
        loop {
            let tx_pkt = match &mut self.tx_pkt {
                Some(x) => x,
                None => return Ok(false)
            };

            let written = if tx_pkt.set_dup && tx_pkt.pos <= 0 {
                self.transport.write(&[tx_pkt.bytes[0] | PublishFlags::DUP], false)?
            } else {
                self.transport.write(&tx_pkt.bytes[tx_pkt.pos..], false)?
            };

            if written <= 0 {
                //Connection closed
                return Ok(true);
            }

            tx_pkt.pos += written;

            if tx_pkt.pos >= tx_pkt.bytes.len() {
                self.transport.flush()?;
                self.clear_tx_packet()?;
                self.last_tx = Instant::now();
            }
        }
    }

    /// The main transceiver code, executed in a loop. Waits for something to be ready and
    /// reacts accordingly. Returns a value controlling the loop.
    async fn task_step(&mut self) -> io::Result<TaskState>
    {
        let wants = self.transport.wants(true, self.tx_pkt.is_some());

        select! {
            ready_write_result = self.transport.ready_for().write(), if wants.write => {
                ready_write_result?;
                self.transport.pre_write()?;

                match self.continue_writing_packet() {
                    Ok(true)  => return Ok(TaskState::ConnectionClosed),
                    Ok(false) => {},
                    Err(err)  => if err.kind() != io::ErrorKind::WouldBlock { return Err(err); }
                }
            },

            ready_read_result = self.transport.ready_for().read(), if wants.read => {
                ready_read_result?;

                if self.transport.pre_read()? {
                    loop {
                        match self.rx_pkt.recv(self.transport.as_mut()) {
                            Ok(PacketReadState::Incoming(pkt)) => if self.process_packet(pkt)? { return Ok(TaskState::WantStop); },
                            Ok(PacketReadState::ConnectionClosed) => return Ok(TaskState::ConnectionClosed),
                            Ok(PacketReadState::NeedMoreData) => {},
                            Err(err) => if err.kind() == io::ErrorKind::WouldBlock { break; } else { return Err(err); }
                        }
                    }
                }
            },

            opt_pkt = self.cmd_queue.recv(), if !self.cmd_queue_closed && self.connected() && self.tx_pkt.is_none() => {
                match opt_pkt {
                    Some(Command::Publish(cmd)) => match cmd.qos {
                        //NOTE: The packet won't be "queued" and rather sent directly because tx_pkt is None

                        QoS::AtMostOnce => self.try_send_packet(cmd.packet, false)?,
                        _ => self.queue_internal_packet_with_timeout(cmd.packet, IdType::publish(cmd.packet_id))?
                    },
                    Some(Command::Subscribe(cmd)) => {
                        if let Some(sub) = self.subscriptions.get_mut(&cmd.topic) {
                            if matches!(cmd.kind, SubscriptionKind::AddRefOnly) {
                                match self.shared.subs.lock().get_mut(&cmd.topic) {
                                    Some(SubscriptionState::Existing(e)) => e.ref_count += 1,
                                    Some(SubscriptionState::Pending(p)) => {
                                        p.ref_count += 1;
                                        panic_in_test!("Subscription for topic \"{}\" exists in Transceiver::subscription, but is pending in ClientShared::subs. Something went wrong.", cmd.topic);
                                    },
                                    None => panic_in_test!("Subscription for topic \"{}\" exists in Transceiver::subscription, but is missing from ClientShared::subs. Something went wrong.", cmd.topic)
                                }
                            } else {
                                sub.add(cmd.kind);
                            }
                        } else {
                            let packet_id = self.shared.next_packet_id.fetch_add(1, Ordering::Relaxed);

                            let pkt = SubscribePacket {
                                packet_id,
                                topics: &[(&cmd.topic, cmd.qos)]
                            }.make_arc_packet();
    
                            self.pending_subs.insert(packet_id, PendingSub {
                                topic: cmd.topic,
                                kind: cmd.kind,
                                packet: pkt.clone()
                            });
    
                            self.try_send_packet(pkt, false)?;
                            self.timeout_queue.push_back(Timeout::now(IdType::subscribe(packet_id)));
                        }
                    },
                    Some(Command::Unsub(cmd)) => {
                        if let Some(sub) = self.subscriptions.get_mut(&cmd.topic) {
                            match cmd.kind {
                                UnsubKind::FastCallback(id) => {
                                    sub.remove_fast_callback(id);

                                    if sub.can_unsub() {
                                        self.unsub_immediate(&cmd.topic, RefCountCheck::Check)?
                                    }
                                },
                                UnsubKind::Immediate => self.unsub_immediate(&cmd.topic, RefCountCheck::Bypass)?
                            }
                        }
                    },
                    Some(Command::Disconnect) => return Ok(TaskState::WantStop),
                    None => self.cmd_queue_closed = true //Channel closed!
                }
            },

            _ = self.alarm.tick() => { //We could put the `self.connected()` condition here, but then we risk having no `select!` branches
                if self.connected() {
                    if self.last_tx.elapsed() >= self.ping_interval {
                        //Even though this is not a TX operation, we need it to prevent spamming ping requests
                        self.last_tx = Instant::now();
                        debug!("Ping");

                        self.queue_internal_packet(self.ping_req.clone(), false)?;
                    }

                    while let Some(to) = self.timeout_queue.front() {
                        if to.t.elapsed() >= self.packet_resend_delay {
                            let to = self.timeout_queue.pop_front().unwrap();

                            if to.packet_id.ty == PacketType::Subscribe {
                                if let Some(sub) = self.pending_subs.get(&to.packet_id.id) {
                                    //Special treatment for the Subscribe packet as we store it in a different hash map
                                    debug!("Re-sending Subscribe packet with ID {}", to.packet_id.id);
                                    self.queue_internal_packet(sub.packet.clone(), false)?;
                                    self.timeout_queue.push_back(to.refresh());
                                }
                            } else {
                                if let Some(pkt) = self.pending_packets.get(&to.packet_id) {
                                    debug!("Re-sending {:?} packet with ID {}", to.packet_id.ty, to.packet_id.id);
                                    self.queue_internal_packet(pkt.clone(), to.packet_id.ty == PacketType::Publish)?;
                                    self.timeout_queue.push_back(to.refresh());
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(TaskState::WantContinue)
    }

    /// A minimal loop to attempt a graceful disconnect. Finishes to transmit
    /// queued packets, including a `DISCONNECT` packet that will be added by
    /// this function.
    async fn send_disconnect_and_flush(&mut self) -> io::Result<()>
    {
        if !self.connected() {
            return Ok(());
        }

        let disconnect_pkt = DisconnectPacket.make_arc_packet();
        self.queue_internal_packet(disconnect_pkt, false)?;

        loop {
            let wants = self.transport.wants(false, self.tx_pkt.is_some());

            if !wants.read && !wants.write {
                break;
            }

            select! {
                ready_write_result = self.transport.ready_for().write(), if wants.write => {
                    ready_write_result?;
                    self.transport.pre_write()?;

                    match self.continue_writing_packet() {
                        Ok(true)  => return Ok(()), //Connection closed
                        Ok(false) => {},
                        Err(err)  => if err.kind() != io::ErrorKind::WouldBlock { return Err(err); }
                    }
                },
    
                ready_read_result = self.transport.ready_for().read(), if wants.read => {
                    ready_read_result?;
                    self.transport.pre_read()?;
                }
            }
        }

        self.disconnect_sent = true;
        Ok(())
    }

    /// A minimal loop to make sure that TLS connection is closed properly. This is does nothing
    /// for plaintext TCP connections.
    async fn send_close_notify(&mut self) -> io::Result<()>
    {
        self.transport.send_close_notify();

        loop {
            let wants = self.transport.wants(false, false);

            if !wants.read && !wants.write {
                return Ok(());
            }

            select! {
                ready_write_result = self.transport.ready_for().write(), if wants.write => {
                    ready_write_result?;
                    self.transport.pre_write()?;
                },
    
                ready_read_result = self.transport.ready_for().read(), if wants.read => {
                    ready_read_result?;
                    self.transport.pre_read()?;
                }
            }
        }
    }

    /// Executes the disconnect sequence (`DISCONNECT` packet & close notification).
    /// 
    /// Reports error in the log and returns `true` if everything went fine and the
    /// connection was closed cleanly.
    async fn disconnect_sequence(&mut self) -> bool
    {
        let mut clean = true;
        
        if let Err(err) = self.send_disconnect_and_flush().await {
            debug!("Error while trying to send disconnect packet: {:?}", err);
            clean = false;
        }

        if let Err(err) = self.send_close_notify().await {
            debug!("Error while trying to send transport close notification: {:?}", err);
            clean = false;
        }

        clean
    }

    /// Initiates the transmission of the `CONNECT` packet and drives the main task loop.
    async fn run_task_loop(&mut self) -> io::Result<bool>
    {
        //Bootstrap connect packet transmission
        if let Some(pkt) = self.tx_pkt.take() {
            self.try_send_packet(pkt.bytes, false)?;
        }

        loop {
            match self.task_step().await? {
                TaskState::ConnectionClosed => return Ok(true),
                TaskState::WantStop         => return Ok(false),
                TaskState::WantContinue     => {}
            }
        }
    }

    /// Marks all the values in a [`NotifierMap`] as failed to make sure that no futures
    /// are stuck when the transceiver task shuts down;
    fn fail_notify_map(map: &NotifierMap, wakers: &mut Vec<Waker>)
    {
        for v in map.lock().values_mut() {
            if let NotifyResult::WithWaker(w) = std::mem::replace(v, NotifyResult::Failed) {
                wakers.push(w);
            }
        }
    }

    /// The entry point of the transceiver task. Runs the main task loop, attempts a graceful
    /// shutdown, ensures that no future is stuck on this task and reports the shutdown state.
    async fn task(&mut self) -> ShutdownStatus
    {
        match self.run_task_loop().await {
            Ok(true) => {
                //Connection got closed; no need to try and disconnect properly because we can't
                return ShutdownStatus { disconnect_sent: false, clean: false };
            },
            Ok(false) => {},
            Err(err) => error!("IO error ocurred in MQTT transceiver task; stopping. The error was {:?}", err)
        }

        //Attempt to disconnect gracefully
        let ret = time::timeout(Duration::from_secs(2), self.disconnect_sequence()).await;

        match ret {
            Ok(true)  => debug!("Disconnected gracefully"),
            Ok(false) => debug!("Disconnected, but parts of the shutdown sequence failed"),
            Err(_)    => debug!("Disconnected, but the shutdown sequence timed out")
        }

        //Make sure that no other task is awaiting on us
        let mut wakers = Vec::new();
        Self::fail_notify_map(&self.shared.notify_ack, &mut wakers);
        Self::fail_notify_map(&self.shared.notify_rec, &mut wakers);
        Self::fail_notify_map(&self.shared.notify_comp, &mut wakers);

        for (_, sub) in self.shared.subs.lock().drain() {
            if let SubscriptionState::Pending(pending_sub) = sub {
                wakers.extend(pending_sub.wakers.into_iter().filter_map(|x| x));
            }
        }

        for waker in wakers {
            waker.wake();
        }

        ShutdownStatus {
            disconnect_sent: self.disconnect_sent,
            clean: ret.unwrap_or(false)
        }
    }
}
