//! Issues:
//! Re-do this whole thing by applying the attribute to a module and
//! putting all rollbackable structs inside that module.
//! That would make it possible to not need to do any recursion for rollback or
//! forget, also eliminating the edge case of setting a rollback implementor who
//! is inside an undo

use std::sync::{Arc, atomic::AtomicUsize};

pub use derive_more::Debug;
pub use macros::rollback;
pub use serde;

#[derive(Clone, Debug, Default)]
pub struct RollbackInfo {
    pub current: Arc<AtomicUsize>,
    pub oldest: Arc<AtomicUsize>,
}

impl RollbackInfo {
    pub fn new() -> Self {
        Self {
            current: Arc::new(AtomicUsize::new(0)),
            oldest: Arc::new(AtomicUsize::new(0)),
        }
    }
}
