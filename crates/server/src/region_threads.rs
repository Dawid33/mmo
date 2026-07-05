//! One OS thread per running region, each with its own tick timer.
//! Fully independent pacing: a slow region ticks late and catches up by
//! skipping missed deadlines; it never blocks the manager or its neighbours.
use std::collections::BTreeMap;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam::channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use game::{RegionCoords, RegionInput, RegionOutput, RegionRunner, RegionSeed, RegionSpawner};

#[derive(Default)]
pub struct ThreadRegionSpawner {
    handles: BTreeMap<RegionCoords, JoinHandle<()>>,
}

impl RegionSpawner for ThreadRegionSpawner {
    fn spawn(
        &mut self,
        id: RegionCoords,
        seed: RegionSeed,
        out: Sender<(RegionCoords, RegionOutput)>,
    ) -> Sender<RegionInput> {
        let (send, recv) = unbounded();
        let handle = std::thread::Builder::new()
            .name(format!("region {},{}", id.x, id.z))
            .spawn(move || {
                // Deserialize/generate on the region's own thread so a big
                // region never stalls the manager.
                let runner = RegionRunner::new(id, seed.into_region(id), out);
                region_thread_loop(runner, recv);
            })
            .expect("failed to spawn region thread");
        self.handles.insert(id, handle);
        send
    }

    fn reap(&mut self, id: RegionCoords) {
        if let Some(handle) = self.handles.remove(&id) {
            // The thread exits right after emitting Stopped (or its channel
            // died); this join is quick.
            if handle.join().is_err() {
                log::error!("region {:?} thread panicked", id);
            }
        }
    }
}

/// recv_deadline is the whole scheduler: handle inputs as they arrive, tick
/// when the deadline fires. Backpressure is inherent — a slow region ticks
/// late; missed deadlines are skipped rather than burst-replayed.
pub fn region_thread_loop(mut runner: RegionRunner, recv: Receiver<RegionInput>) {
    let tick = Duration::from_millis(game::TICK_RATE);
    let mut next = Instant::now() + tick;
    loop {
        match recv.recv_deadline(next) {
            Ok(input) => {
                if !runner.handle_input(input) {
                    return; // Shutdown acknowledged with Stopped
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                runner.tick();
                next += tick;
                let now = Instant::now();
                if next < now {
                    // Fell behind (heavy tick / scheduler stall): skip the
                    // missed deadlines instead of spiralling.
                    next = now + tick;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}
