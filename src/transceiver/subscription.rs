use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use log::warn;

use super::util::panic_in_test;
use super::packets::IncomingPublishPacket;
use super::commands::{FastCallback, SubscriptionKind};

/// Store a subscription's MPSC queues and fast callbacks.
#[derive(Default)]
pub struct Subscription
{
    /// Lossy subscriptions: unordered [`Vec`] of tokio's [`mpsc::Sender`].
    /// The transceiver task will skip queues where messages can't be sent
    /// immediately (using [`mpsc::Sender::try_send()`]).
    lossy_queue: Vec<mpsc::Sender<IncomingPublishPacket>>,

    /// Unbounded subscriptions: unordered [`Vec`] of tokio's [`mpsc::UnboundedSender`].
    /// Messages will always be pushed onto these queues.
    unbounded_queue: Vec<mpsc::UnboundedSender<IncomingPublishPacket>>,

    /// [`Vec`] of callback functions ordered by their IDs.
    fast_callbacks: Vec<FastCallback>
}

/// Calls a predicate on each values of the provided [`Vec`], and removes
/// the elements where the predicate returned `true` using [`Vec::swap_remove()`].
/// 
/// This is faster but does not preserve ordering.
fn prune<T, P: FnMut(&T) -> bool>(vec: &mut Vec<T>, mut predicate: P)
{
    let mut i = 0;

    while i < vec.len() {
        if predicate(&vec[i]) {
            vec.swap_remove(i);
        } else {
            i += 1;
        }
    }
}

impl Subscription
{
    /// Ensures that there a no more queues or fast callbacks in this subscription. That still does not
    /// mean the transceiver can unsubscribe automatically; `ref_count` needs to be checked in the
    /// client's shared data.
    #[inline]
    pub fn can_unsub(&self) -> bool
    {
        self.lossy_queue.is_empty() && self.unbounded_queue.is_empty() && self.fast_callbacks.is_empty()
    }

    /// Dispatches a message to all queues and callbacks in this subscription. Queues that are closed
    /// are removed from this susbscription object.
    /// 
    /// The message is cloned for each recipient, but that shouldn't be a problem since
    /// [`IncomingPublishPacket`] is in fact a wrapped [`std::sync::Arc`].
    pub fn dispatch(&mut self, msg: &IncomingPublishPacket)
    {
        prune(&mut self.lossy_queue, |queue| {
            matches!(queue.try_send(msg.clone()), Err(TrySendError::Closed(_)))
        });

        prune(&mut self.unbounded_queue, |queue| {
            queue.send(msg.clone()).is_err()
        });

        for cb in &mut self.fast_callbacks {
            (cb.f)(msg.clone());
        }
    }

    fn add_fast_callback(&mut self, new_cb: FastCallback)
    {
        if self.fast_callbacks.last().map(|last_cb| new_cb.id > last_cb.id).unwrap_or(true) {
            //This will cover 99% of the cases
            self.fast_callbacks.push(new_cb);
        } else if let Err(index) = self.fast_callbacks.binary_search_by_key(&new_cb.id, |x| x.id) {
            //This will cover the remaining cases (race condition between acquiring the id and queuing the subscribe command)
            self.fast_callbacks.insert(index, new_cb);
        } else {
            //...and this should just never happen!
            panic_in_test!("Duplicate fast callback with ID {}", new_cb.id);
        }
    }

    pub fn remove_fast_callback(&mut self, id: u32)
    {
        if let Ok(index) = self.fast_callbacks.binary_search_by_key(&id, |x| x.id) {
            self.fast_callbacks.remove(index);
        } else {
            warn!("Fast callback with ID {} not found", id);
        }
    }

    /// Inserts the contents of the specified [`SubscriptionKind`] in its approriate
    /// [`Vec`]. Note that [`SubscriptionKind::AddRefOnly`]`does nothing as it should
    /// be added to the `subs` map of the client's shared data struct.
    pub fn add(&mut self, kind: SubscriptionKind)
    {
        match kind {
            SubscriptionKind::AddRefOnly       => {},
            SubscriptionKind::Lossy(queue)     => self.lossy_queue.push(queue),
            SubscriptionKind::Unbounded(queue) => self.unbounded_queue.push(queue),
            SubscriptionKind::FastCallback(cb) => self.add_fast_callback(cb)
        }
    }
}
