//! The compute pool: Web Workers running [`ComputeJob`]s — the browser's
//! stand-in for Bevy's `AsyncComputeTaskPool` threads (`renderer::compute`).
//!
//! Started on first use with [`super::compute_worker_count`] workers and kept
//! for the page's life (across sessions). A job goes to the worker with the
//! fewest in flight as `[id: u64 LE][kind: u8][bincode input]`; the answer
//! comes back as `[id: u64 LE][bincode output]` and resolves the job's
//! [`Reply`]. A worker that dies (its `error` event: a panic in a job traps
//! its instance) is terminated, and its unanswered jobs run on the page
//! instead, so no task is ever left hanging.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use js_sys::Uint8Array;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};

use crate::renderer::compute::{ComputeJob, JobKind, run_encoded};

/// Header bytes of a job message: id + kind.
const JOB_HEADER: usize = 9;
/// Header bytes of an answer: id.
const REPLY_HEADER: usize = 8;

// ---------------------------------------------------------------- worker side

/// The worker's half: answer every job message.
pub fn worker_start() {
    let scope: web_sys::DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
    let on_message =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let Some(bytes) = super::message_bytes(&event) else {
                return;
            };
            if bytes.len() < JOB_HEADER {
                log::error!("[compute-worker] short message");
                return;
            }
            let Some(kind) = JobKind::from_u8(bytes[8]) else {
                log::error!("[compute-worker] unknown job kind {}", bytes[8]);
                return;
            };
            let output = run_encoded(kind, &bytes[JOB_HEADER..]);
            let mut reply = Vec::with_capacity(REPLY_HEADER + output.len());
            reply.extend_from_slice(&bytes[..REPLY_HEADER]);
            reply.extend_from_slice(&output);
            let scope: web_sys::DedicatedWorkerGlobalScope = js_sys::global().unchecked_into();
            let _ = super::post_bytes(&scope, &Uint8Array::from(reply.as_slice()));
        });
    scope.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    on_message.forget();
}

// ------------------------------------------------------------------ page side

/// Where a job's answer lands.
#[derive(Default)]
struct Slot {
    output: Option<Vec<u8>>,
    waker: Option<Waker>,
}

/// Resolves to the job's encoded output.
struct RawReply(Rc<RefCell<Slot>>);

impl Future for RawReply {
    type Output = Vec<u8>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<u8>> {
        let mut slot = self.0.borrow_mut();
        match slot.output.take() {
            Some(output) => Poll::Ready(output),
            None => {
                slot.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// A submitted job's output, decoded on the page.
pub struct Reply<J: ComputeJob> {
    raw: RawReply,
    _job: std::marker::PhantomData<fn() -> J>,
}

impl<J: ComputeJob> Future for Reply<J> {
    type Output = J::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<J::Output> {
        Pin::new(&mut self.raw)
            .poll(cx)
            .map(|bytes| crate::renderer::compute::decode_output::<J>(&bytes))
    }
}

fn resolve(slot: &Rc<RefCell<Slot>>, output: Vec<u8>) {
    let waker = {
        let mut slot = slot.borrow_mut();
        slot.output = Some(output);
        slot.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// A job a worker has not answered yet (kept so it can run on the page if
/// the worker dies).
struct Pending {
    worker: usize,
    kind: JobKind,
    input: Vec<u8>,
    slot: Rc<RefCell<Slot>>,
}

struct WorkerEntry {
    worker: web_sys::Worker,
    in_flight: usize,
    alive: bool,
    _handlers: (
        Closure<dyn FnMut(web_sys::MessageEvent)>,
        Closure<dyn FnMut(JsValue)>,
    ),
}

#[derive(Default)]
struct PoolState {
    workers: Vec<WorkerEntry>,
    pending: BTreeMap<u64, Pending>,
    next_id: u64,
}

thread_local! {
    /// `None` until first use; `Some(None)` = no pool (workers off / failed).
    static POOL: RefCell<Option<Option<Rc<RefCell<PoolState>>>>> = const { RefCell::new(None) };
}

fn start_pool() -> Option<Rc<RefCell<PoolState>>> {
    if !super::enabled() {
        return None;
    }
    let count = super::compute_worker_count();
    if count == 0 {
        return None;
    }
    let state = Rc::new(RefCell::new(PoolState::default()));
    for index in 0..count {
        let worker = match super::spawn("compute", &format!("orion-compute-{index}")) {
            Ok(worker) => worker,
            Err(e) => {
                log::error!("[web-worker] compute worker {index} failed to start: {e:?}");
                continue;
            }
        };
        let on_message = {
            let state = Rc::downgrade(&state);
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                let (Some(state), Some(bytes)) = (state.upgrade(), super::message_bytes(&event))
                else {
                    return;
                };
                if bytes.len() < REPLY_HEADER {
                    return;
                }
                let id = u64::from_le_bytes(bytes[..REPLY_HEADER].try_into().unwrap());
                let pending = {
                    let mut state = state.borrow_mut();
                    let pending = state.pending.remove(&id);
                    if let Some(p) = &pending {
                        state.workers[p.worker].in_flight -= 1;
                    }
                    pending
                };
                if let Some(pending) = pending {
                    resolve(&pending.slot, bytes[REPLY_HEADER..].to_vec());
                }
            })
        };
        let on_error = {
            let state = Rc::downgrade(&state);
            Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                let Some(state) = state.upgrade() else { return };
                log::error!(
                    "[web-worker] compute worker {index} died: {}",
                    super::error_text(&event)
                );
                worker_died(&state, index);
            })
        };
        worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        state.borrow_mut().workers.push(WorkerEntry {
            worker,
            in_flight: 0,
            alive: true,
            _handlers: (on_message, on_error),
        });
    }
    let started = state.borrow().workers.len();
    log::info!("[web-worker] compute pool: {started} workers");
    (started > 0).then_some(state)
}

/// Terminate worker `index` and run its unanswered jobs on the page.
fn worker_died(state: &Rc<RefCell<PoolState>>, index: usize) {
    let orphans: Vec<Pending> = {
        let mut state = state.borrow_mut();
        let entry = &mut state.workers[index];
        if !entry.alive {
            return;
        }
        entry.alive = false;
        entry.in_flight = 0;
        entry.worker.terminate();
        let ids: Vec<u64> = state
            .pending
            .iter()
            .filter(|(_, p)| p.worker == index)
            .map(|(&id, _)| id)
            .collect();
        ids.into_iter()
            .filter_map(|id| state.pending.remove(&id))
            .collect()
    };
    for orphan in orphans {
        resolve(&orphan.slot, run_encoded(orphan.kind, &orphan.input));
    }
}

fn pool() -> Option<Rc<RefCell<PoolState>>> {
    POOL.with(|pool| pool.borrow_mut().get_or_insert_with(start_pool).clone())
}

/// Post `job` to the least busy worker; `Err(job)` when there is no pool (the
/// caller runs it on the page).
pub fn submit<J: ComputeJob>(job: J) -> Result<Reply<J>, J> {
    let Some(state) = pool() else { return Err(job) };
    let mut state = state.borrow_mut();
    let Some(index) = (0..state.workers.len())
        .filter(|&i| state.workers[i].alive)
        .min_by_key(|&i| state.workers[i].in_flight)
    else {
        return Err(job);
    };
    let input = crate::renderer::compute::encode_job(&job);
    let id = state.next_id;
    state.next_id += 1;
    let message = Uint8Array::new_with_length((JOB_HEADER + input.len()) as u32);
    message.subarray(0, 8).copy_from(&id.to_le_bytes());
    message.subarray(8, 9).copy_from(&[J::KIND as u8]);
    message
        .subarray(JOB_HEADER as u32, message.length())
        .copy_from(&input);
    if let Err(e) = super::post_bytes(&state.workers[index].worker, &message) {
        log::error!("[web-worker] posting a job failed: {e:?}");
        return Err(job);
    }
    let slot = Rc::new(RefCell::new(Slot::default()));
    state.workers[index].in_flight += 1;
    state.pending.insert(
        id,
        Pending {
            worker: index,
            kind: J::KIND,
            input,
            slot: slot.clone(),
        },
    );
    Ok(Reply {
        raw: RawReply(slot),
        _job: std::marker::PhantomData,
    })
}
