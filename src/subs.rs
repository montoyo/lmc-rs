use std::sync::Arc;
use tokio::sync::mpsc;

use super::shared::{ClientShared, SubscriptionState};
use super::transceiver::commands::{Command, UnsubCommand, UnsubKind};

/// A kind of subscription that cannot be used to obtain message, but
/// is used to prevent the transceiver task from automatically
/// unsubscribing from a topic.
/// 
/// When an instance of this struct is dropped, the reference is
/// released and the subscription is not guaranteed to be held
/// anymore, unless of course [`Hold::leak()`] is called.
/// 
/// In any case, this can be bypassed by explicitely calling
/// [`super::Client::unsubscribe()`].
pub struct Hold
{
    shared: Arc<ClientShared>,
    topic: String,
    leaked: bool
}

impl Hold
{
    /// Creates a [`Hold`] susbcription object.
    /// 
    /// Note that an entry corresponding to this topic must already exist
    /// in the subscription map and that the reference should have already
    /// been added.
    pub(super) fn new(shared: Arc<ClientShared>, topic: String) -> Self
    {
        Self { shared, topic, leaked: false }
    }

    /// Leaks the subscription object, preventing the transceiver task from
    /// unsubscribing to the topic forever.
    /// 
    /// This can still be bypassed by explicitely calling [`super::Client::unsubscribe()`].
    pub fn leak(mut self)
    {
        self.leaked = true;
    }
}

impl Drop for Hold
{
    fn drop(&mut self)
    {
        if !self.leaked {
            match self.shared.subs.lock().get_mut(&self.topic) {
                Some(SubscriptionState::Existing(data)) => data.ref_count -= 1,
                Some(SubscriptionState::Pending(data)) => data.ref_count -= 1,
                None => {}
            }
        }
    }
}

/// A reference to a "fast callback" subscription that can be used to remove
/// the said callback.
/// 
/// Note that if this value is dropped before calling [`Callback::remove_callback()`],
/// there will be no way to remove it anymore and the client will never automatically
/// unsubscribe from the corresponding topic.
/// 
/// However, it will still be possible to unsubscribe from the topic manually using
/// [`super::Client::unsubscribe()`].
pub struct Callback
{
    cmd_queue: mpsc::Sender<Command>,
    topic: String,
    id: u32
}

impl Callback
{
    /// Creates a [`Callback`] susbcription object.
    /// 
    /// Note that an entry corresponding to this topic must already exist
    /// in the subscription map and that callback should have already
    /// been registered.
    pub(super) fn new(cmd_queue: mpsc::Sender<Command>, topic: String, id: u32) -> Self
    {
        Self { cmd_queue, topic, id }
    }

    /// Removes the callback from the susbcription.
    /// To completely unsubscribe from a topic, use [`super::Client::unsubscribe()`].
    pub async fn remove_callback(self)
    {
        let cmd = UnsubCommand { topic: self.topic, kind: UnsubKind::FastCallback(self.id) };
        let _ = self.cmd_queue.send(cmd.into()).await; //Only possible error is that the channel has been closed, meaning we're unsubscribed anyway
    }
}
