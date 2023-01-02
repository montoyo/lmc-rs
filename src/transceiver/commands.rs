use std::sync::Arc;
use tokio::sync::mpsc;

use crate::QoS;
use super::packets::IncomingPublishPacket;
use super::util::def_enum_with_intos;

/// A fast callback (boxed function) with its u32 ID attached.
pub struct FastCallback
{
    pub id: u32,
    pub f: Box<dyn FnMut(IncomingPublishPacket) + Send + Sync>
}

/// All the different ways to subscribe to a topic
pub enum SubscriptionKind
{
    /// Used by so-called "Hold" subscriptions, this will register the
    /// subscription with the broker (if it wasn't registered already)
    /// and hold it until the reference count goes back to zero.
    AddRefOnly,

    /// Lossy subscriptions use a bounded MPSC channel to handle messages.
    /// If the queue is full, the message will be dropped (which is why it's
    /// called "Lossy" subscriptions).
    Lossy(mpsc::Sender<IncomingPublishPacket>),

    /// Unbounded subscription use an unbounded MPSC channel to handle
    /// messages. This is the safest, most convenient way to receive
    /// topic messages as it will never fail.
    Unbounded(mpsc::UnboundedSender<IncomingPublishPacket>),

    /// Fast callbacks are thread-safe functions that are not supposed to
    /// block. They will be called for each message received in a topic.
    FastCallback(FastCallback)
}

/// Instructs the transceiver task to publish the packet provided in the
/// [`PublishCommand::packet`] field.
pub struct PublishCommand
{
    pub packet_id: u16,
    pub qos: QoS,
    pub packet: Arc<[u8]>
}

/// Instructs the transceiver task to create the subscription provided in
/// the [`SubscribeCommand::kind`] field and to register it with the broker
/// if needed.
pub struct SubscribeCommand
{
    pub topic: String,
    pub qos: QoS,
    pub kind: SubscriptionKind
}

/// Ways to unsubscribe from a topic. Only [`UnsubKind::Immediate`] will
/// cause the transceiver task to send an `UNSUB` packet immediately.
/// Other variants will only do it if nothing else relies on that
/// susbcription.
/// 
/// This is a smaller set than [`SubscriptionKind`] because other types
/// of subscriptions can either be closed by closing the MPSC channel or
/// by modifying the client's shared data directly.
pub enum UnsubKind
{
    /// Removes a [`FastCallback`] with the specified ID
    FastCallback(u32),

    /// Unsusbcribe from the topic immediately, no matter of how many
    /// objects (fast callbacks, queues, etc.) rely on that subscription.
    Immediate
}

/// Instructs the transceiver task to remove a subscription. Based on
/// the value of [`UnsubCommand::kind`] and on the existence of other
/// subscription objects, the task may or may not send an `UNSUB`
/// packet when processing this command.
pub struct UnsubCommand
{
    pub topic: String,
    pub kind: UnsubKind
}

def_enum_with_intos! {
    /// Wraps all the possible commands that can be sent to the
    /// transceiver task.
    pub enum Command
    {
        Publish(PublishCommand),
        Subscribe(SubscribeCommand),
        Unsub(UnsubCommand),

        /// Instructs the transceiver task to disconnect gracefully
        /// (if possible) and then stop.
        Disconnect
    }
}
