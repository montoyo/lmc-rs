use std::task::Waker;
use std::sync::atomic::{AtomicU16, AtomicU32};

use parking_lot::Mutex;
use fxhash::FxHashMap;

use super::QoS;
use super::transceiver::util::def_enum_with_intos;

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
pub type NotifierMap = Mutex<FxHashMap<u16, NotifyResult>>;

/// A subscription that has been validated by the broker
pub struct ExistingSubscription
{
    /// The number of references keeping the subscribtion alive.
    /// 
    /// If this value drops down to zero, and if there are no
    /// other reasons (such as alive MPSC channels) to keep
    /// the subscriptions alive, the transceiver task will
    /// automatically unsubscribe from the topic.
    pub ref_count: i32,

    /// The [`QoS`] of the subscription, as returned by the
    /// server in the `SUBACK` packet.
    pub qos: QoS
}

/// A subscription that has yet to be validated by the broker
/// and that can be awaited on.
pub struct PendingSusbcription
{
    /// The number of references that will keep the subscribtion
    /// alive. It has no real meaning while in [`PendingSusbcription`],
    /// however this value will be transferred over to
    /// [`ExistingSubscription`].
    pub ref_count: i32,

    /// A list of wakers awaiting this subscription. Values should
    /// never be removed from this list as futures use the waker
    /// index to update it. Instead, values can be changed to [`None`].
    pub wakers: Vec<Option<Waker>>
}

impl PendingSusbcription
{
    /// Instantiates a [`PendingSusbcription`] with a single reference
    /// and no wakers.
    pub fn with_one_ref() -> Self
    {
        Self { ref_count: 1, wakers: Vec::new() }
    }
}

def_enum_with_intos! {
    /// The value of an entry in the subscription map, describing the
    /// state of the susbcription.
    pub enum SubscriptionState
    {
        /// The subscription that has been validated by the broker
        Existing(ExistingSubscription),

        /// The subscription has yet to be validated by the broker
        Pending(PendingSusbcription)
    }
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
    pub subs: Mutex<FxHashMap<String, SubscriptionState>>,

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
            notify_ack: Mutex::new(Default::default()),
            notify_rec: Mutex::new(Default::default()),
            notify_comp: Mutex::new(Default::default()),
            subs: Mutex::new(Default::default()),
            next_callback_id: AtomicU32::new(0)
        }
    }
}
