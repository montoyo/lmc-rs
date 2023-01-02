use std::sync::{Arc, Weak as WeakArc};
use std::panic::Location;
use std::time::{Instant, Duration};
use std::thread;

use parking_lot::Mutex;
use fxhash::FxHashMap;
use log::debug;

struct Entry
{
    state: &'static str,
    location: Option<&'static Location<'static>>,
    t: Instant
}

type Shared = FxHashMap<&'static str, Entry>;

/// A utility class used in tests only that prints out a status
/// report every 10 seconds.
/// 
/// This is used to track blocking async functions, which I
/// found to be extremely difficult to do because the stack
/// traces presented in debuggers is unreadable. Tokio's
/// `console_subscriber` helps a bit but not enough.
/// 
/// [`TracingUtility`] is ref-counted and will stop
/// automatically once all references are released.
#[derive(Clone)]
pub struct TracingUtility(Arc<Mutex<Shared>>);

impl TracingUtility
{
    fn dump_state(shared: &Shared)
    {
        for (&k, v) in shared {
            if let Some(location) = v.location {
                debug!("{}: {} @ {}:{} for {:.3}s", k, v.state, location.file(), location.line(), v.t.elapsed().as_secs_f32());
            } else {
                debug!("{}: {} for {:.3}s", k, v.state, v.t.elapsed().as_secs_f32());
            }
        }
    }

    fn thread_fn(shared_weak: WeakArc<Mutex<Shared>>)
    {
        loop {
            thread::sleep(Duration::from_secs(10));

            let shared = match shared_weak.upgrade() {
                Some(x) => x,
                None => break
            };

            debug!("================================== STATUS REPORT ==================================");
            Self::dump_state(&shared.lock());
            debug!("===================================================================================");
        }
    }

    /// Creates a new [`TracingUtility`], spawns its thread and returns it.
    /// 
    /// The thread will stop once the return value and all of its clones
    /// are dropped.
    pub fn spawn() -> Self
    {
        let ret = Arc::new(Mutex::new(Default::default()));
        let weak = Arc::downgrade(&ret);

        thread::spawn(move || Self::thread_fn(weak));
        Self(ret)
    }

    /// Updates the state and the location of the tracking entry with the specified key.
    pub fn trace(&self, key: &'static str, state: &'static str, location: &'static Location<'static>)
    {
        self.0.lock().insert(key, Entry { state, location: Some(location), t: Instant::now() });
    }

    /// Updates the state string only of the tracking entry with the specified key, leaving
    /// its location unchanged.
    pub fn update_state(&self, key: &'static str, state: &'static str)
    {
        let t = Instant::now();

        self.0
            .lock()
            .entry(key)
            .and_modify(|e| { e.state = state; e.t = t; })
            .or_insert(Entry { state, location: None, t });
    }

    /// Binds this [`TracingUtility`] to the tracking entry with the specified key,
    /// returning a [`KeyedTracingUtility`].
    pub fn with_key(self, key: &'static str) -> KeyedTracingUtility
    {
        KeyedTracingUtility {
            tracing_utility: self,
            key
        }
    }
}

/// A reference to a [`TracingUtility`] bound to a specific tracking entry.
/// Can be created by calling [`TracingUtility::with_key()`].
pub struct KeyedTracingUtility
{
    tracing_utility: TracingUtility,
    key: &'static str
}

impl KeyedTracingUtility
{
    /// Calls [`TracingUtility::trace()`] with the key bound to this object.
    pub fn trace(&self, state: &'static str, location: &'static Location<'static>)
    {
        self.tracing_utility.trace(self.key, state, location);
    }

    /// Calls [`TracingUtility::update_state()`] with the key bound to this object.
    pub fn update_state(&self, state: &'static str)
    {
        self.tracing_utility.update_state(self.key, state);
    }
}
