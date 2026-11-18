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

/// Why a host query (or a fallible command's check) did not answer a value.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryError {
    /// An error the function declares (`raises`): a `WireValue::Variant` naming it. The script takes its
    /// fallible path (`?`, `else`, `match`). An error the function does not declare is the fault
    /// `HostFault`.
    Raised(WireValue),
    /// A host bug (bad arguments, no answer): the fault `HostFault` with this message.
    Failed(String),
}

pub trait Host {
    /// Read-only, answered at once; a pure function of the arguments and the host's state this tick. Only a
    /// query that declares errors may answer `Raised`.
    fn query(&mut self, f: u16, args: &[WireValue]) -> Result<WireValue, QueryError>;
    /// Checks the fallible command `f` against this tick's state when it is issued (a pure read, like a
    /// query): `Ok` buffers it, `Raised` takes the script's fallible path and buffers nothing. Never asked
    /// of a command that declares no errors.
    fn check(&mut self, f: u16, args: &[WireValue]) -> Result<(), QueryError>;
    /// World-changing call, handed over when its segment commits, in the order issued. A command that
    /// passed its check may still fail when applied (the state changed within the segment): the host
    /// reports that through its own error channel (spec §6.6), never to the script.
    fn command(&mut self, f: u16, args: &[WireValue]);
    /// A promise request, handed over when its segment commits. Answer later with `Vm::resolve`.
    fn request(&mut self, id: PromiseId, f: u16, args: &[WireValue]);
    fn print(&mut self, text: &str);
    fn report(&mut self, report: Report);
    /// A committed `Enqueue` of a script event no handler of this instance takes and another instance's
    /// does (a part's `emit` for the home, say): the host routes it, by the event's qualified name (`Program::events`), to the instance
    /// that has a handler for it.
    fn route(&mut self, event: &str, value: &WireValue);
}

/// The errors the VM itself fails an awaited promise with, whatever the host function declares: `Lost` (the
/// host lost the request, or answered an error the program does not know), `Timeout` (no answer within
/// `PROMISE_TIMEOUT_TICKS`), `WrongType` (an answer that does not fit the promised type). All are data-less;
/// the compiler adds them to every awaited result's error set.
pub const PROMISE_ERRORS: [&str; 3] = ["Lost", "Timeout", "WrongType"];

/// The namespace of the host side of the quest builtins (`quest.joined`, `quest.complete`, …): the compiler
/// lowers to it; no script can name it.
pub const QUEST_NAMESPACE: &str = "quest";

#[derive(Clone, Debug, PartialEq)]
pub struct HostApi {
    pub version: u32,
    pub structs: Vec<HostStruct>,
    /// The host's enums (data-less variants only); a value is `WireValue::Variant` named `Enum::Variant`.
    pub enums: Vec<HostEnum>,
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

/// A host enum: its variants carry no data (`Occupation::Smith`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEnum {
    pub name: String,
    pub variants: Vec<String>,
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
    /// Read as `x.name`, without parentheses: a receiver `Query` with no parameters.
    pub property: bool,
    /// A query the coordinator can evaluate: the only kind allowed in the query subset (`where`,
    /// `requires`).
    pub fact: bool,
    /// The `Int` parameters a call may leave out (it passes the rest by name), and the value each takes then.
    pub defaults: Vec<(String, i64)>,
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
    /// A host enum, by name.
    Enum(String),
    /// Any storable value.
    Value,
    /// The receiving type.
    Infer,
    /// Any event struct.
    Event,
    /// A dialogue or scene (the compiler's `Dialogue` type): its qualified name on the wire.
    Dialogue,
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

// ---- a program against the API --------------------------------------------------------------------------

impl crate::program::Program {
    /// Whether the program fits `api` beyond its version (spec §6.4): every host site names a fn of the
    /// API, of its kind, with its parameters (the receiver first) and result, declaring exactly its errors
    /// (none for a promise, whose errors are the promise's own); every host `emit` of a dialogue names an
    /// event of the API with an argument of its type per field. A host binds no program that does not:
    /// the VM trusts a site's kind and errors when it calls the host (`Host::check` only for a fallible
    /// site), and the dialogue runner a host event's index.
    pub fn check_api(&self, api: &HostApi) -> Result<(), crate::program::ProgramError> {
        use crate::dialogue::StepDef;
        let bad = |why: String| Err(crate::program::ProgramError::Invalid(why));
        let types = &self.types;
        for (i, s) in self.host_sites.iter().enumerate() {
            let Some(hf) = api.fns.get(usize::from(s.host_fn)) else {
                return bad(format!(
                    "host site {i} names host fn {}, which the API lacks",
                    s.host_fn
                ));
            };
            let name = &hf.name;
            if s.kind != hf.kind {
                return bad(format!(
                    "host site {i} calls `{name}` as another kind of fn"
                ));
            }
            let receiver = hf.receiver.iter().map(|r| receiver_ty(api, r));
            let want: Vec<HostTy> = receiver
                .chain(hf.params.iter().map(|p| p.1.clone()))
                .collect();
            if s.params.len() != want.len()
                || !s
                    .params
                    .iter()
                    .zip(&want)
                    .all(|(&t, h)| host_ty_fits(types, h, t))
            {
                return bad(format!("host site {i} passes `{name}` other parameters"));
            }
            let ret = match (s.kind, types.desc(s.ret)) {
                (HostFnKind::Command, Ok(crate::types::TypeDesc::Unit)) => true,
                (HostFnKind::Command, _) => false,
                (HostFnKind::Promise, Ok(crate::types::TypeDesc::Promise(v))) => {
                    host_ty_fits(types, &hf.ret, *v)
                }
                _ => host_ty_fits(types, &hf.ret, s.ret),
            };
            if !ret {
                return bad(format!("host site {i} takes another result from `{name}`"));
            }
            let mut declared = hf.raises.clone();
            declared.sort_unstable();
            let mut raises = s.raises.clone();
            raises.sort_unstable();
            let fits = match s.kind {
                HostFnKind::Promise => raises.is_empty(),
                _ => raises == declared,
            };
            if !fits {
                return bad(format!("host site {i} declares other errors than `{name}`"));
            }
        }
        for s in &self.dialogues.steps {
            let StepDef::Emit {
                event,
                script: false,
                args,
            } = s
            else {
                continue;
            };
            let Some(e) = api.events.get(usize::from(*event)) else {
                return bad(format!(
                    "an emit names host event {event}, which the API lacks"
                ));
            };
            let fits = e.fields.len() == args.len()
                && args.iter().zip(&e.fields).all(|(&f, (_, h))| {
                    self.fns
                        .get(usize::from(f))
                        .and_then(|f| f.sig.as_ref())
                        .is_some_and(|sig| host_ty_fits(types, h, sig.ret))
                });
            if !fits {
                return bad(format!("an emit of `{}` passes other fields", e.name));
            }
        }
        Ok(())
    }
}

/// The type of a receiver named `name`: the API's struct or enum of that name (anything when it has none).
fn receiver_ty(api: &HostApi, name: &str) -> HostTy {
    if api.structs.iter().any(|s| s.name == name) {
        HostTy::Struct(name.to_string())
    } else if api.enums.iter().any(|e| e.name == name) {
        HostTy::Enum(name.to_string())
    } else {
        HostTy::Infer
    }
}

/// Whether the program's type `t` is one a value of host type `h` takes: the same shape, a struct or enum
/// by its name, anything for an open type (`Value`, `Infer`, `Event`).
fn host_ty_fits(types: &crate::types::TypeTable, h: &HostTy, t: crate::types::TypeId) -> bool {
    use crate::types::TypeDesc as D;
    let Ok(d) = types.desc(t) else {
        return false;
    };
    match (h, d) {
        (HostTy::Value | HostTy::Infer | HostTy::Event, _) => true,
        (HostTy::Unit, D::Unit)
        | (HostTy::Int, D::Int)
        | (HostTy::Float, D::Float)
        | (HostTy::Bool, D::Bool)
        | (HostTy::Str | HostTy::Dialogue, D::Str) => true,
        (HostTy::List(h), D::List(e)) => host_ty_fits(types, h, *e),
        (HostTy::Option(h), D::Enum { name, variants }) => {
            name == "Option"
                && matches!(variants.as_slice(), [(none, n), (some, s)]
                    if none == "None" && n.is_empty() && some == "Some"
                        && matches!(s.as_slice(), [(_, v)] if host_ty_fits(types, h, *v)))
        }
        (HostTy::Struct(n), D::Struct { name, .. }) | (HostTy::Enum(n), D::Enum { name, .. }) => {
            n == name
        }
        _ => false,
    }
}
