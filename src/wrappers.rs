//! The purpose of this module is to provide the [`LmcHashMap`] and [`LmcMutex`]
//! wrappers to handle the presence/abscence of `fxhash` and `parking_lot` as
//! they are optional dependencies.

pub use fxhash::FxHashMap as LmcHashMap;
pub use parking_lot::Mutex as LmcMutex;
