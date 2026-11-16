#![forbid(unsafe_code)]

pub mod alloc;
pub mod consts;
pub mod fault;
pub mod hash;
pub mod layout;
pub mod list;
pub mod mem;
pub mod object;
pub mod store;
pub mod string;

pub use fault::{FaultKind, VmResult};
pub use hash::Fnv64;
pub use layout::{Layout, LoadError};
pub use mem::{Checkpoint, Mem, MemError, RollbackError};
pub use store::Store;
