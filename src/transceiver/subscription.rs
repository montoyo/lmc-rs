use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use log::warn;

use super::util::panic_in_test;
use super::packets::IncomingPublishPacket;
use super::commands::{FastCallback, SubscriptionKind};
use crate::wrappers::LmcHashMap;

/// Store a subscription's MPSC queues and fast callbacks.
/// Leaf in the subscriptions tree.
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
    fast_callbacks: Vec<FastCallback>,

    /// Flag to indicate that the client is currently subscribed to the topic.
    /// If false, all collections (`lossy_queue`, `unbounded_queue`, `fast_callbacks`)
    /// should be empty.
    subscribed: bool
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
            SubscriptionKind::Void             => {},
            SubscriptionKind::Lossy(queue)     => self.lossy_queue.push(queue),
            SubscriptionKind::Unbounded(queue) => self.unbounded_queue.push(queue),
            SubscriptionKind::FastCallback(cb) => self.add_fast_callback(cb)
        }
    }

    /// Removes all subscriptions associated with this subscription object and
    /// clears the `subscribed` flag.
    fn clear(&mut self)
    {
        self.lossy_queue.clear();
        self.unbounded_queue.clear();
        self.fast_callbacks.clear();
        self.subscribed = false;
    }

    pub fn set_subscribed(&mut self)
    {
        self.subscribed = true;
    }

    pub fn is_subscribed(&self) -> bool
    {
        self.subscribed
    }
}

/// Utility struct to split topic strings on slashes. `head`
/// contains the first component, while `tail` contains the
/// rest of the topic path.
#[derive(Clone, Copy)]
struct TopicSlicer<'a>
{
    head: &'a str,
    tail: Option<&'a str>
}

impl<'a> TopicSlicer<'a>
{
    fn new(s: &'a str) -> Self
    {
        if let Some(head_pos) = s.find('/') {
            let head = &s[..head_pos];
            let tail = &s[head_pos + 1..];

            Self { head, tail: Some(tail) }
        } else {
            Self { head: s, tail: None }
        }
    }

    fn tail(self) -> Option<Self>
    {
        self.tail.map(Self::new)
    }
}

/// The first piece of a node in the subscription tree
#[derive(Default)]
struct SubscriptionSetNode
{
    /// Entries that correspond to a regular (non-wildcard) path component.
    exact: LmcHashMap<String, Box<SubscriptionSetEntry>>,

    /// Entry that corresponds to the wildcard ('*') path component, if any.
    any: Option<Box<SubscriptionSetEntry>>,

    /// Subscription object (leaf) that corresponds to the recursive wildcard
    /// ('#') path component.
    any_recursive: Subscription
}

/// The second piece of a node in the subscription tree
#[derive(Default)]
struct SubscriptionSetEntry
{
    /// The subscription object (leaf) corresponding to this node
    sub: Subscription,

    /// Child nodes
    children: SubscriptionSetNode
}

impl SubscriptionSetEntry
{
    fn dispatch(&mut self, tail: Option<TopicSlicer>, msg: &IncomingPublishPacket)
    {
        if let Some(sub_topic) = tail {
            self.children.dispatch(sub_topic, msg);
        } else {
            self.sub.dispatch(msg);
        }
    }
}

impl SubscriptionSetNode
{
    fn dispatch(&mut self, topic: TopicSlicer, msg: &IncomingPublishPacket)
    {
        //We assume that `elements.len() > 0`
        self.any_recursive.dispatch(msg);

        if let Some(any) = &mut self.any {
            any.dispatch(topic.tail(), msg);
        }

        if let Some(exact) = self.exact.get_mut(topic.head) {
            exact.dispatch(topic.tail(), msg);
        }
    }

    fn get_or_create_exact(&mut self, k: &str) -> &mut SubscriptionSetEntry
    {
        /*
        //So, Rust can be really retarded sometimes and this does not work:
        if let Some(exact) = self.exact.get_mut(k) {
            return exact;
        }

        //The only alternative (apparently) is this:
        if self.exact.contains_key(k) {
            return self.exact.get_mut(k).unwrap();
        }

        //...which causes two hashmap lookups, which I don't like, so might as well just do this:
        */

        self.exact
            .entry(k.to_string())
            .or_insert_with(|| Box::new(Default::default()))
    }
}

/// Root struct of the subscription tree
#[derive(Default)]
pub struct SubscriptionSet
{
    root: SubscriptionSetNode
}

impl SubscriptionSet
{
    /// Dispatches a message to the receivers matching the message's topic.
    pub fn dispatch(&mut self, msg: &IncomingPublishPacket)
    {
        let topic = TopicSlicer::new(msg.topic());
        self.root.dispatch(topic, msg);
    }

    /// Gets the subscription object corresponding the provided topic pattern.
    /// Creates it if it doesn't already exist.
    /// 
    /// Returns `Err(())` if the topic pattern is invalid (for instance, if a '#'
    /// is used before the end of the topic path).
    pub fn get_or_create(&mut self, topic: &str) -> Result<&mut Subscription, ()>
    {
        let mut topic_slicer = TopicSlicer::new(topic);
        let mut node = &mut self.root;

        while let Some(tail) = topic_slicer.tail() {
            if topic_slicer.head == "#" {
                return Err(());
            }

            node = if topic_slicer.head == "*" {
                &mut node.any.get_or_insert_with(|| Box::new(Default::default())).as_mut().children
            } else {
                &mut node.get_or_create_exact(topic_slicer.head).children
            };

            topic_slicer = tail;
        }

        if topic_slicer.head == "#" {
            Ok(&mut node.any_recursive)
        } else if topic_slicer.head == "*" {
            let entry = node.any.get_or_insert_with(|| Box::new(Default::default())).as_mut();
            Ok(&mut entry.sub)
        } else {
            let entry = node.get_or_create_exact(topic_slicer.head);
            Ok(&mut entry.sub)
        }
    }

    /// Gets the subscription object corresponding the provided topic pattern.
    /// 
    /// Returns `Ok(Some(_))` if the subscription object exists, `Ok(None)` if
    /// it does not, and `Err(())` if the topic pattern is invalid (for instance,
    /// if a '#' is used before the end of the topic path).
    /// 
    /// Note that if a value is returned, it doesn't necessarily mean that the
    /// client has subscribed to this topic. For that, check
    /// [`Subscription::is_subscribed()`].
    pub fn get(&mut self, topic: &str) -> Result<Option<&mut Subscription>, ()>
    {
        let mut topic_slicer = TopicSlicer::new(topic);
        let mut node = &mut self.root;

        while let Some(tail) = topic_slicer.tail() {
            if topic_slicer.head == "#" {
                return Err(());
            }

            node = if topic_slicer.head == "*" {
                match &mut node.any {
                    Some(entry) => &mut entry.children,
                    None => return Ok(None)
                }
            } else {
                match node.exact.get_mut(topic_slicer.head) {
                    Some(entry) => &mut entry.children,
                    None => return Ok(None)
                }
            };

            topic_slicer = tail;
        }

        if topic_slicer.head == "#" {
            Ok(Some(&mut node.any_recursive))
        } else if topic_slicer.head == "*" {
            Ok(node.any.as_mut().map(|entry| &mut entry.sub))
        } else {
            Ok(node.exact.get_mut(topic_slicer.head).map(|entry| &mut entry.sub))
        }
    }

    /// Removes all subscriptions contained in the subscription object corresponding
    /// the provided topic pattern.
    /// 
    /// Returns `Ok(true)` if the subscription object exists, was subscribed and was
    /// cleared, `Ok(false)` otherwise or `Err(())` if the topic pattern is invalid
    /// (for instance, if a '#' is used before the end of the topic path).
    /// 
    /// This function has yet to delete intermediate nodes if they do not contain
    /// any subscription. This will be fixed in a future version of LMC.
    pub fn clear(&mut self, topic: &str) -> Result<bool, ()>
    {
        //TODO: Delete useless nodes

        if let Some(sub) = self.get(topic)? {
            if sub.is_subscribed() {
                sub.clear();
                return Ok(true);
            }
        }

        Ok(false)
    }
}
