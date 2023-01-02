//! The purpose of this module is to provide the [`LmcHashMap`] and [`LmcMutex`]
//! wrappers to handle the presence/abscence of `fxhash` and `parking_lot` as
//! they are optional dependencies.

#[cfg(feature = "fxhash")] pub use fxhash::FxHashMap as LmcHashMap;
#[cfg(not(feature = "fxhash"))] pub use std::collections::HashMap as LmcHashMap;
#[cfg(feature = "parking_lot")] pub use parking_lot::Mutex as LmcMutex;

#[cfg(not(feature = "parking_lot"))]
use std::sync::{Mutex, MutexGuard};

#[cfg(not(feature = "parking_lot"))]
pub struct LmcMutex<T>(Mutex<T>);

#[cfg(not(feature = "parking_lot"))]
impl<T> LmcMutex<T>
{
    pub fn new(t: T) -> Self
    {
        Self(Mutex::new(t))
    }

    pub fn lock(&self) -> MutexGuard<T>
    {
        self.0.lock().unwrap()
    }
}
