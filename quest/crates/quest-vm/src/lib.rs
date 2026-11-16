#![forbid(unsafe_code)]

pub mod alloc;
pub mod asm;
pub mod bytecode;
pub mod consts;
pub mod dict;
pub mod eq;
pub mod fault;
pub mod fmt;
pub mod fuel;
pub mod hash;
pub mod host;
pub mod layout;
pub mod list;
pub mod mem;
pub mod object;
pub mod program;
pub mod store;
pub mod string;
pub mod types;
pub mod wire;

pub use fault::{FaultKind, VmResult};
pub use hash::Fnv64;
pub use asm::ProgramBuilder;
pub use bytecode::{BYTECODE_VERSION, Builtin, Instr, Op};
pub use host::{Host, HostApi};
pub use program::{Program, ProgramError};
pub use layout::{Layout, LoadError};
pub use mem::{Checkpoint, Mem, MemError, RollbackError};
pub use store::Store;
