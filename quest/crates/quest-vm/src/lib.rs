#![forbid(unsafe_code)]

pub mod consts;
pub mod hash;
pub mod mem;

pub use hash::Fnv64;
pub use mem::{Checkpoint, Mem, MemError, RollbackError};
