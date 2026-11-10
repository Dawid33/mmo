//! Web Workers for the browser build: the multithreading the wasm client has.
//!
//! Native runs the sim on its own threads and meshing on Bevy's compute pool;
//! on wasm32 Bevy's task pools are single-threaded (they run on the page's
//! event loop), so without this everything shares the one main thread with
//! rendering. Shared-memory wasm threads (`+atomics`, `-Zbuild-std`, COOP/COEP)
//! are not an option for this client: wgpu drops `Send`/`Sync` from its types
//! on wasm whenever the `atomics` target feature is on, and Bevy's renderer
//! requires them. So the workers here are **separate wasm instances that talk
//! by message passing** — the same seam the project already keeps between its
//! threads (channels, never shared memory):
//!
//! - every worker instantiates the SAME compiled module the page runs
//!   (`index.html` compiles it once and publishes it as
//!   `globalThis.__orionWasm`; it is posted to each worker, so nothing is
//!   fetched or compiled twice). wasm-bindgen's start function runs `main` in
//!   the worker too, which returns at once there ([`in_worker`]); the worker's
//!   bootstrap script then calls the exported `orion_worker_main(role, base)`
//!   (`main.rs`), which lands in [`worker_main`];
//! - messages are bincode bytes in a transferred `ArrayBuffer`
//!   ([`post_bytes`] / [`message_bytes`]);
//! - roles: [`sim`] (the game manager + the WebTransport connection — the
//!   native game thread) and [`pool`] (the compute pool: meshing jobs,
//!   Bevy's `AsyncComputeTaskPool` on native).
//!
//! `?workers=0` in the page URL turns all of it off (everything back on the
//! main thread, as before); `?workers=N` sizes the compute pool. A page without
//! `__orionWasm` (some other loader) falls back the same way.

pub mod pool;
pub mod sim;

use std::sync::{Mutex, OnceLock};

use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};

/// Upper bound on compute workers. Each one is a whole instance of the client
/// module (its own linear memory: tens of MB), so more is not free.
pub const MAX_COMPUTE_WORKERS: usize = 6;
/// Logical cores left to the page (main thread) and the sim worker when the
/// compute pool is sized from `navigator.hardwareConcurrency`.
pub const CORES_RESERVED: usize = 2;

/// True inside a Web Worker (any of ours): `main` returns at once there.
pub fn in_worker() -> bool {
    // `WorkerGlobalScope` exists only as a worker global; `instanceof` on a
    // missing constructor would throw, so look the name up instead.
    Reflect::has(&js_sys::global(), &"WorkerGlobalScope".into()).unwrap_or(false)
        && web_sys::window().is_none()
}

/// The `?workers=` page knob: `None` = unset (auto), `Some(0)` = off.
fn workers_param() -> Option<usize> {
    let search = web_sys::window()?.location().search().ok()?;
    let query = search.trim_start_matches('?');
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "workers")
        .and_then(|(_, value)| value.parse().ok())
}

/// The compiled module + loader the page published (`index.html`).
struct Boot {
    js: JsValue,
    module: JsValue,
    base: JsValue,
}

fn boot() -> Option<Boot> {
    let boot = Reflect::get(&js_sys::global(), &"__orionWasm".into()).ok()?;
    if boot.is_undefined() || boot.is_null() {
        return None;
    }
    let get = |key: &str| Reflect::get(&boot, &key.into()).ok().filter(|v| !v.is_undefined());
    Some(Boot { js: get("js")?, module: get("module")?, base: get("base")? })
}

/// Whether workers run here: on the page (not in a worker), with the module
/// published, and not turned off by `?workers=0`.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let on = !in_worker() && workers_param() != Some(0) && boot().is_some();
        log::info!("[web-worker] workers {}", if on { "on" } else { "off (main thread only)" });
        on
    })
}

/// Compute workers to start: `?workers=N`, else the logical cores less
/// [`CORES_RESERVED`], clamped to `1..=MAX_COMPUTE_WORKERS`.
pub fn compute_worker_count() -> usize {
    if let Some(n) = workers_param() {
        return n.min(MAX_COMPUTE_WORKERS);
    }
    let cores = web_sys::window().map(|w| w.navigator().hardware_concurrency()).unwrap_or(4.0) as usize;
    cores.saturating_sub(CORES_RESERVED).clamp(1, MAX_COMPUTE_WORKERS)
}

/// The worker bootstrap (a module worker from a Blob URL). Its first message is
/// `{ js, module, role, base }`: it imports the page's wasm-bindgen loader,
/// instantiates the posted module, and calls `orion_worker_main(role, base)`,
/// which installs the Rust `onmessage`. Messages that arrive meanwhile are
/// queued and replayed into it, so the page may post right after spawning. A
/// failed start is rethrown outside the promise, which raises the `error`
/// event on the page's `Worker`.
const BOOTSTRAP_JS: &str = r#"
let queue = [];
let booted = false;
self.onmessage = async (event) => {
    if (booted) { queue.push(event); return; }
    booted = true;
    try {
        const { js, module, role, base } = event.data;
        const wasm = await import(js);
        await wasm.default({ module_or_path: module });
        wasm.orion_worker_main(role, base);
        const handler = self.onmessage;
        for (const queued of queue) handler(queued);
        queue = null;
    } catch (error) {
        setTimeout(() => { throw error; });
    }
};
"#;

/// The Blob URL of [`BOOTSTRAP_JS`], made once.
fn bootstrap_url() -> Result<String, JsValue> {
    thread_local! {
        static URL: std::cell::OnceCell<String> = const { std::cell::OnceCell::new() };
    }
    URL.with(|cell| {
        if let Some(url) = cell.get() {
            return Ok(url.clone());
        }
        let parts = Array::of1(&JsValue::from_str(BOOTSTRAP_JS));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("text/javascript");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        let _ = cell.set(url.clone());
        Ok(url)
    })
}

/// Start a worker playing `role` (`"sim"` or `"compute"`); `name` shows in the
/// browser's devtools. Posting to it right away is fine (the bootstrap queues).
pub fn spawn(role: &str, name: &str) -> Result<web_sys::Worker, JsValue> {
    let boot = boot().ok_or_else(|| JsValue::from_str("no __orionWasm published by the page"))?;
    let options = web_sys::WorkerOptions::new();
    options.set_type(web_sys::WorkerType::Module);
    options.set_name(name);
    let worker = web_sys::Worker::new_with_options(&bootstrap_url()?, &options)?;
    let init = js_sys::Object::new();
    Reflect::set(&init, &"js".into(), &boot.js)?;
    Reflect::set(&init, &"module".into(), &boot.module)?;
    Reflect::set(&init, &"role".into(), &role.into())?;
    Reflect::set(&init, &"base".into(), &boot.base)?;
    worker.post_message(&init)?;
    Ok(worker)
}

/// The page's base URL inside a worker (relative URLs there would resolve
/// against the bootstrap's Blob URL); `None` on the page itself.
static PAGE_BASE: OnceLock<String> = OnceLock::new();

/// `relative` resolved against the page (works on the page and in workers).
pub fn page_url(relative: &str) -> String {
    match PAGE_BASE.get() {
        Some(base) => web_sys::Url::new_with_base(relative, base).map(|u| u.href()).unwrap_or_else(|_| relative.into()),
        None => relative.into(),
    }
}

/// `fetch` on whichever global this runs in.
pub fn fetch(url: &str) -> js_sys::Promise {
    match web_sys::window() {
        Some(window) => window.fetch_with_str(url),
        None => js_sys::global().unchecked_into::<web_sys::WorkerGlobalScope>().fetch_with_str(url),
    }
}

/// `setTimeout(callback, ms)` on whichever global this runs in.
pub fn set_timeout(callback: &js_sys::Function, ms: i32) {
    let _ = match web_sys::window() {
        Some(window) => window.set_timeout_with_callback_and_timeout_and_arguments_0(callback, ms),
        None => js_sys::global()
            .unchecked_into::<web_sys::WorkerGlobalScope>()
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback, ms),
    };
}

/// Resolves after `ms` milliseconds (page or worker).
pub async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| set_timeout(&resolve, ms));
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// bincode-encode `value` into a fresh `Uint8Array` (one copy out of linear
/// memory; the buffer is then transferred, not copied again).
pub fn encode<T: serde::Serialize>(value: &T) -> Uint8Array {
    let bytes = bincode::serialize(value).expect("worker message encodes");
    Uint8Array::from(bytes.as_slice())
}

/// Post `bytes` to `target` (a `Worker`, or the worker's own global scope),
/// transferring its buffer.
pub fn post_bytes(target: &JsValue, bytes: &Uint8Array) -> Result<(), JsValue> {
    let transfer = Array::of1(&bytes.buffer());
    if let Some(worker) = target.dyn_ref::<web_sys::Worker>() {
        worker.post_message_with_transfer(bytes, &transfer)
    } else {
        target
            .unchecked_ref::<web_sys::DedicatedWorkerGlobalScope>()
            .post_message_with_transfer(bytes, &transfer)
    }
}

/// The bytes of a message posted with [`post_bytes`].
pub fn message_bytes(event: &web_sys::MessageEvent) -> Option<Vec<u8>> {
    event.data().dyn_into::<Uint8Array>().ok().map(|a| a.to_vec())
}

/// A readable line for a worker `error` event.
pub fn error_text(event: &JsValue) -> String {
    match event.dyn_ref::<web_sys::ErrorEvent>() {
        Some(e) => format!("{} ({}:{})", e.message(), e.filename(), e.lineno()),
        None => "worker failed to start".into(),
    }
}

/// Holds page-thread-only JS handles (`Worker`, `Closure`) inside Bevy
/// resources, which must be `Send`.
pub struct MainThreadOnly<T>(pub T);
// SAFETY: this build has no shared-memory threads (no `atomics` target
// feature: wgpu/Bevy forbid it, see the module doc), so nothing can ever move
// the value to another thread; workers are separate instances with their own
// memory.
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Send for MainThreadOnly<T> {}
#[cfg(not(target_feature = "atomics"))]
unsafe impl<T> Sync for MainThreadOnly<T> {}

/// One log record emitted inside a worker.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogLine {
    pub level: u8,
    pub target: String,
    pub message: String,
}

impl LogLine {
    fn level(&self) -> log::Level {
        match self.level {
            1 => log::Level::Error,
            2 => log::Level::Warn,
            3 => log::Level::Info,
            4 => log::Level::Debug,
            _ => log::Level::Trace,
        }
    }

    /// Re-emit on the page, through its logger (Bevy's: the console, and
    /// `just web`'s terminal forwarding), so worker lines read like the rest.
    pub fn emit(&self) {
        log::log!(target: &self.target, self.level(), "{}", self.message);
    }
}

/// Lines a forwarding worker (the sim) buffered for the page; `None` = the
/// worker logs to its own console.
static FORWARDED: Mutex<Option<Vec<LogLine>>> = Mutex::new(None);

/// Take the lines buffered since the last call (forwarding workers).
pub fn take_log_lines() -> Vec<LogLine> {
    FORWARDED.lock().unwrap().as_mut().map(std::mem::take).unwrap_or_default()
}

struct WorkerLogger {
    role: &'static str,
}

impl log::Log for WorkerLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // The page's filter (main.rs `log_filter`) keeps INFO for the game's
        // crates; the opt-in DEBUG `perf_budget` lines stay off here.
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = LogLine {
            level: record.level() as u8,
            target: record.target().to_string(),
            message: record.args().to_string(),
        };
        if let Some(buffer) = FORWARDED.lock().unwrap().as_mut() {
            buffer.push(line);
            return;
        }
        let text = JsValue::from_str(&format!("[{}] {} {}: {}", self.role, record.level(), line.target, line.message));
        match record.level() {
            log::Level::Error => web_sys::console::error_1(&text),
            log::Level::Warn => web_sys::console::warn_1(&text),
            _ => web_sys::console::log_1(&text),
        }
    }

    fn flush(&self) {}
}

/// The worker's entry (`orion_worker_main` in `main.rs`): panics to the
/// console, a logger, the block registry (the sim's colliders and the
/// meshers read the installed block properties), then the role.
pub fn worker_main(role: &str, base: String) {
    console_error_panic_hook::set_once();
    let _ = PAGE_BASE.set(base);
    let role: &'static str = match role {
        "sim" => "sim-worker",
        "compute" => "compute-worker",
        other => panic!("unknown worker role {other:?}"),
    };
    if role == "sim-worker" {
        *FORWARDED.lock().unwrap() = Some(Vec::new());
    }
    static LOGGER: OnceLock<WorkerLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| WorkerLogger { role });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    // The `[perf]` timeline buffers until `init`; this worker's events go out
    // from here (forwarded to the page by the sim worker).
    game::perf::init(role);
    crate::blocks::load_registry();
    match role {
        "sim-worker" => sim::worker_start(),
        _ => pool::worker_start(),
    }
}
