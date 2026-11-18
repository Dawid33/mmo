//! The fuel table (design D6). Part of the bytecode version: changing a value means bumping it.
//!
//! "+ size" in the docs is `ceil(words / 8)` of what is copied, scanned, hashed or compared.

/// Loads, copies, arithmetic, comparisons, jumps, `GetTag`, `ListLen`, `TracePush`, `Not`.
pub const FUEL_SIMPLE: u64 = 1;
/// `Call`, `Ret`.
pub const FUEL_CALL: u64 = 2;
/// `Dup`, `Move`, `Drop`, field/index get/take/set, `MakeUnique` (+ size), reference ops (+1 per path step).
pub const FUEL_HEAP: u64 = 3;
/// `NewObj`, `NewList`, `NewDict`, `LoadStr` (+ size), `RefMake` and extensions.
pub const FUEL_ALLOC: u64 = 5;
/// Dict key ops (+ key size).
pub const FUEL_KEY: u64 = 6;
/// Every builtin (+ size; `DictNext`, `DictKeyAt` and `DictValAt` count the positions their search probes).
pub const FUEL_BUILTIN: u64 = 4;
/// `CallHost` of a query (+ argument size).
pub const FUEL_HOST_QUERY: u64 = 10;
/// `CallHost` of a command (+ argument size).
pub const FUEL_HOST_COMMAND: u64 = 10;
/// Added to `CallHost` of a fallible command: the host checks it when it is issued.
pub const FUEL_HOST_CHECK: u64 = 10;
/// `CallHost` of a promise request (+ argument size).
pub const FUEL_HOST_REQUEST: u64 = 20;
/// Parking a task (+ frame words).
pub const FUEL_AWAIT: u64 = 20;
/// Restoring a parked task (+ frame words).
pub const FUEL_RESUME: u64 = 20;
/// Starting a handler (+ payload size).
pub const FUEL_DISPATCH: u64 = 10;
/// One reap step.
pub const FUEL_REAP: u64 = 2;
/// `Enqueue` (+ payload size).
pub const FUEL_ENQUEUE: u64 = 10;
/// `After` (+ payload size).
pub const FUEL_AFTER: u64 = 10;
/// `CancelTimer`, `CancelStageTimers` (a scan of the timer table).
pub const FUEL_CANCEL: u64 = 5;
