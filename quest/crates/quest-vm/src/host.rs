//! The host interface (design D9): types only. The VM calls a [`Host`]; the embedder describes what it
//! offers with a [`HostApi`] and the compiler checks scripts against it.

pub use crate::fault::FaultKind;
use crate::wire::WireValue;

/// The id of an outstanding promise request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PromiseId(pub u64);

/// A source position, for traces and reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrcLoc {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

pub trait Host {
    /// Read-only, answered at once; a pure function of the arguments and the host's state this tick.
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, String>;
    /// World-changing call, handed over when its segment commits, in the order issued.
    fn command(&mut self, f: u16, args: &[WireValue]);
    /// A promise request, handed over when its segment commits. Answer later with `Vm::resolve`.
    fn request(&mut self, id: PromiseId, f: u16, args: &[WireValue]);
    fn print(&mut self, text: &str);
    fn report(&mut self, report: Report);
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostApi {
    pub version: u32,
    pub structs: Vec<HostStruct>,
    /// The events the host delivers, with their fields (a handler's parameter, `emit`'s argument).
    pub events: Vec<HostStruct>,
    pub fns: Vec<HostFn>,
    pub errors: Vec<HostError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostStruct {
    pub name: String,
    pub fields: Vec<(String, HostTy)>,
}

/// An error the host can raise; `fields` are its data (empty for a data-less error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    pub name: String,
    pub fields: Vec<(String, HostTy)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFn {
    pub namespace: Option<String>,
    pub receiver: Option<String>,
    pub name: String,
    pub params: Vec<(String, HostTy)>,
    pub ret: HostTy,
    pub raises: Vec<String>,
    pub kind: HostFnKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFnKind {
    Query,
    Command,
    Promise,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostTy {
    Unit,
    Int,
    Float,
    Bool,
    Str,
    List(Box<HostTy>),
    Option(Box<HostTy>),
    Struct(String),
    /// Any storable value.
    Value,
    /// The receiving type.
    Infer,
    /// Any event struct.
    Event,
}

/// Why `deliver` did not run an event (design D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    NoHandler,
    UnknownEvent,
    WrongShape,
    InstanceFailed,
    QueueFull,
    InboxFull,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    EventQueued { event: String },
    EventDropped { event: String, reason: DropReason },
    HandlerAborted { task: String, cause: AbortCause },
    InitFailed { global: String, cause: AbortCause },
}

#[derive(Clone, Debug, PartialEq)]
pub enum AbortCause {
    Error {
        name: String,
        data: WireValue,
        trace: Vec<SrcLoc>,
        trace_dropped: u32,
    },
    Fault {
        kind: FaultKind,
        message: String,
        stack: Vec<SrcLoc>,
    },
}
