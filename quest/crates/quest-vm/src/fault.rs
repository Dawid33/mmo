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
    /// A structural walk (compare, hash, lookup) would visit more than `WORK_WORDS_MAX` words.
    ValueTooLarge,
    BadFloatToInt,
    AssertFailed,
    TooManyCommands,
    TooManyPromises,
    TooManyParked,
    /// `after` with `TIMERS_MAX` timers live.
    TooManyTimers,
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

impl FaultKind {
    /// A short human text for the kind (a fault's default message).
    pub fn describe(self) -> &'static str {
        match self {
            FaultKind::IndexOutOfRange => "index out of range",
            FaultKind::MissingKey => "missing key",
            FaultKind::IntOverflow => "integer overflow",
            FaultKind::DivByZero => "division by zero",
            FaultKind::BadRef => "reference no longer resolves",
            FaultKind::HeapFull => "heap full",
            FaultKind::ValueTooLarge => "value too large",
            FaultKind::BadFloatToInt => "float is NaN or out of the int range",
            FaultKind::AssertFailed => "assertion failed",
            FaultKind::TooManyCommands => "too many commands",
            FaultKind::TooManyPromises => "too many promises",
            FaultKind::TooManyParked => "too many parked handlers",
            FaultKind::TooManyTimers => "too many timers",
            FaultKind::PromiseAlreadyAwaited => "promise already awaited",
            FaultKind::HostFault => "host fault",
            FaultKind::Corrupt => "corrupt memory",
        }
    }
}
