//! Faults a running script can raise (design D9) and the result type of heap operations.

use crate::alloc::AllocError;
use crate::mem::MemError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultKind {
    IndexOutOfRange,
    MissingKey,
    IntOverflow,
    DivByZero,
    BadRef,
    HeapFull,
    BadFloatToInt,
    AssertFailed,
    TooManyCommands,
    TooManyPromises,
    TooManyParked,
    PromiseAlreadyAwaited,
    HostFault,
    /// Memory that does not parse: sets the instance `Failed`, no abort is attempted.
    Corrupt,
}

pub type VmResult<T> = Result<T, FaultKind>;

impl From<MemError> for FaultKind {
    fn from(_: MemError) -> FaultKind {
        FaultKind::Corrupt
    }
}

impl From<AllocError> for FaultKind {
    fn from(_: AllocError) -> FaultKind {
        FaultKind::HeapFull
    }
}
