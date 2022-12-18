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

    pub fn spawn() -> Self
    {
        let ret = Arc::new(Mutex::new(Default::default()));
        let weak = Arc::downgrade(&ret);

        thread::spawn(move || Self::thread_fn(weak));
        Self(ret)
    }

    pub fn trace(&self, key: &'static str, state: &'static str, location: &'static Location<'static>)
    {
        self.0.lock().insert(key, Entry { state, location: Some(location), t: Instant::now() });
    }

    pub fn update_state(&self, key: &'static str, state: &'static str)
    {
        let t = Instant::now();

        self.0
            .lock()
            .entry(key)
            .and_modify(|e| { e.state = state; e.t = t; })
            .or_insert(Entry { state, location: None, t });
    }

    pub fn with_key(self, key: &'static str) -> KeyedTracingUtility
    {
        KeyedTracingUtility {
            tracing_utility: self,
            key
        }
    }
}

pub struct KeyedTracingUtility
{
    tracing_utility: TracingUtility,
    key: &'static str
}

impl KeyedTracingUtility
{
    pub fn trace(&self, state: &'static str, location: &'static Location<'static>)
    {
        self.tracing_utility.trace(self.key, state, location);
    }

    pub fn update_state(&self, state: &'static str)
    {
        self.tracing_utility.update_state(self.key, state);
    }
}
