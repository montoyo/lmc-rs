use std::task::Waker;
use std::sync::atomic::{AtomicU16, AtomicU32};

use crate::QoS;
use crate::wrappers::{LmcHashMap, LmcMutex};

/// Describes awaiting states associated with a packet ID in a [`NotifierMap`]
pub enum NotifyResult
{
    /// The packet ID will be awaited on but has no [`Waker`] yet.
    /// 
    /// This value is set by the future.
    WithoutWaker,

    /// The packet ID is being awaited on and the specified [`Waker`]
    /// should be woken up when the event is triggered.
    /// 
    /// This value is set by the future.
    WithWaker(Waker),

    /// The packet ID might not have been sent because the
    /// transceiver task ended.
    /// 
    /// This value is set by the transceiver task.
    Failed
}

/// A [`NotifierMap`] is used to await specific publish events.
/// 
/// The key is the ID of the publish packet and the value is
/// essentially used to provide a reference to the [`Waker`].
/// 
/// The entry is initially created by the future, so if the
/// entry corresponding to a publish packet is removed, that
/// means that the event was triggered (or that the future
/// got cancelled).
pub type NotifierMap = LmcMutex<LmcHashMap<u16, NotifyResult>>;

/// The value of an entry in the subscription map, describing the
/// state of the susbcription.
pub enum SubscriptionState
{
    /// The subscription exists and has been validated by the broker.
    /// The value is the [`QoS`] of the subscription as defined by
    /// the broker.
    Existing(QoS),

    /// The subscription has yet to be validated by the broker.
    /// The value is a list of wakers waiting on the subscription
    /// to be established.
    Pending(Vec<Option<Waker>>)
}

/// Shared information between the [`super::Client`] and the
/// transceiver task. Generally wrapped in an [`std::sync::Arc`].
pub struct ClientShared
{
    /// Value to be used for the next identifiable packet
    pub next_packet_id: AtomicU16,

    /// [`NotifierMap`] for the `PUBACK` stage
    pub notify_ack: NotifierMap,

    /// [`NotifierMap`] for the `PUBREC` stage
    pub notify_rec: NotifierMap,

    /// [`NotifierMap`] for the `PUBCOMP` stage
    pub notify_comp: NotifierMap,

    /// A map describing the state of each topic subscriptions
    pub subs: LmcMutex<LmcHashMap<String, SubscriptionState>>,

    /// Value to be used as an ID for the next "fast callback"
    /// topic subscription.
    pub next_callback_id: AtomicU32
}

impl ClientShared
{
    /// Instantiates a new [`ClientShared`] with the starting
    /// packet/callback IDs and empty notify/subscription maps.
    pub fn new() -> Self
    {
        Self {
            next_packet_id: AtomicU16::new(1),
            notify_ack: LmcMutex::new(Default::default()),
            notify_rec: LmcMutex::new(Default::default()),
            notify_comp: LmcMutex::new(Default::default()),
            subs: LmcMutex::new(Default::default()),
            next_callback_id: AtomicU32::new(0)
        }
    }
}
