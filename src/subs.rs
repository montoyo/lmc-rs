use tokio::sync::mpsc;
use super::transceiver::commands::{Command, UnsubCommand, UnsubKind};

/// A reference to a "fast callback" subscription that can be used to remove
/// the said callback.
/// 
/// Note that if this value is dropped before calling [`Callback::remove_callback()`],
/// there will be no way to remove it anymore.
/// 
/// Also note that since v0.2, removing a callback will never cause the client to
/// unsubscribe from the topic. Instead,
/// [`Client::unsubscribe()`](super::Client::unsubscribe()) should be used to
/// unsubscribe from a topic.
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
    /// To completely unsubscribe from a topic, use
    /// [`Client::unsubscribe()`](super::Client::unsubscribe()).
    pub async fn remove_callback(self)
    {
        let cmd = UnsubCommand { topic: self.topic, kind: UnsubKind::FastCallback(self.id) };
        let _ = self.cmd_queue.send(cmd.into()).await; //Only possible error is that the channel has been closed, meaning we're unsubscribed anyway
    }
}
