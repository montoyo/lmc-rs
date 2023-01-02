use std::future::Future;
use std::task::{Poll, Context};
use std::sync::Arc;
use std::pin::Pin;

use crate::QoS;
use crate::shared::{ClientShared, NotifierMap, SubscriptionState, NotifyResult};

/// A trait used to statically index a [`NotifierMap`] in [`ClientShared`]
pub(super) trait NotifierMapAccessor: Unpin
{
    fn access_notifier_map(&self) -> &NotifierMap;
}

macro_rules! def_notifier_acessors {
    ($($name:ident => |$var:ident| $expr:expr),+) => {
        $(
            pub struct $name(pub(super) Arc<ClientShared>);

            impl NotifierMapAccessor for $name
            {
                fn access_notifier_map(&self) -> &NotifierMap { let $var = &self.0; $expr }
            }
        )+
    };
}

def_notifier_acessors! {
    AckNotifierMapAccessor  => |shared| &shared.notify_ack,
    RecNotifierMapAccessor  => |shared| &shared.notify_rec,
    CompNotifierMapAccessor => |shared| &shared.notify_comp
}

/// A future that can be used to await any of the message publish stages:
/// - `PUBACK` using [`AckNotifierMapAccessor`]
/// - `PUBREC` using [`RecNotifierMapAccessor`]
/// - `PUBCOMP` using [`CompNotifierMapAccessor`]
/// 
/// Can be cancelled safely.
pub(super) struct PublishFuture<NMA: NotifierMapAccessor>
{
    packet_id: u16,
    notifier: NMA,
    result: Option<bool>
}

impl<NMA: NotifierMapAccessor> PublishFuture<NMA>
{
    /// Instantiates a [`PublishFuture`] for the specified packet ID and
    /// publish stage. Automatically registers itself into the corresponding
    /// [`NotifierMap`].
    pub fn new(packet_id: u16, notifier: NMA) -> Self
    {
        notifier.access_notifier_map().lock().insert(packet_id, NotifyResult::WithoutWaker);

        Self {
            packet_id,
            notifier,
            result: None
        }
    }
}

impl<NMA: NotifierMapAccessor> Future for PublishFuture<NMA>
{
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool>
    {
        if let Some(ret) = self.result {
            return Poll::Ready(ret);
        }

        let mut map = self.notifier.access_notifier_map().lock();

        match map.get_mut(&self.packet_id) {
            Some(entry) => match entry {
                NotifyResult::Failed => {
                    drop(map);
                    self.as_mut().result = Some(true);
                    Poll::Ready(true)
                },
                _ => {
                    *entry = NotifyResult::WithWaker(cx.waker().clone());
                    Poll::Pending
                }
            },
            None => {
                drop(map);
                self.as_mut().result = Some(false);
                Poll::Ready(false)
            }
        }
    }
}

impl<NMA: NotifierMapAccessor> Drop for PublishFuture<NMA>
{
    fn drop(&mut self)
    {
        if self.result.is_none() {
            self.notifier.access_notifier_map().lock().remove(&self.packet_id);
        }
    }
}

/// A future that can be used to await a `SUBACK` packet for the
/// specified topic.
/// 
/// Can be cancelled safely.
pub(super) struct SubscribeFuture<'a>
{
    client_shared: &'a ClientShared,
    topic: &'a str,
    waker_index: Option<usize>,
    result: Option<Result<QoS, ()>>
}

impl<'a> SubscribeFuture<'a>
{
    /// Instantiates a [`SubscribeFuture`] for the specified topic. Note that
    /// an entry for the corresponding topic in the subscription map should
    /// exist prior to a call to this function or at least, prior to awaiting
    /// the returned future.
    pub(super) fn new(client_shared: &'a ClientShared, topic: &'a str) -> Self
    {
        Self {
            client_shared, topic,
            waker_index: None,
            result: None
        }
    }
}

impl<'a> Future for SubscribeFuture<'a>
{
    type Output = Result<QoS, ()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<QoS, ()>>
    {
        if let Some(result) = self.result {
            return Poll::Ready(result);
        }

        let mut sub_map = self.client_shared.subs.lock();

        let sub_state = match sub_map.get_mut(self.topic) {
            Some(x) => x,
            None => {
                //Subscription failed
                drop(sub_map);
                self.as_mut().result = Some(Err(()));
                return Poll::Ready(Err(()));
            }
        };

        match sub_state {
            SubscriptionState::Existing(data) => {
                //Subscription succeeded
                let qos = data.qos;
                drop(sub_map);
                self.as_mut().result = Some(Ok(qos));

                Poll::Ready(Ok(qos))
            },
            SubscriptionState::Pending(data) => {
                let waker = Some(cx.waker().clone());

                if let Some(i) = self.waker_index {
                    data.wakers[i] = waker;
                } else {
                    let i = data.wakers.len();
                    data.wakers.push(waker);

                    drop(sub_map);
                    self.as_mut().waker_index = Some(i);
                }

                Poll::Pending
            }
        }
    }
}

impl<'a> Drop for SubscribeFuture<'a>
{
    fn drop(&mut self)
    {
        if self.result.is_none() {
            if let Some(i) = self.waker_index {
                match self.client_shared.subs.lock().get_mut(self.topic) {
                    Some(SubscriptionState::Pending(data)) => data.wakers[i] = None,
                    _ => {}
                }
            }
        }
    }
}
