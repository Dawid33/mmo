//! [`Runner`]: drives a `Vm` against a [`StubHost`] one tick at a time.

use std::sync::Arc;

use quest_vm::LoadError;
use quest_vm::host::Report;
use quest_vm::program::Program;
use quest_vm::vm::{Delivery, TickReport, Vm, VmConfig};
use quest_vm::wire::WireValue;

use crate::api::stub_api;
use crate::host::{StubConfig, StubHost};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Quiescent after `ticks` steps.
    Finished {
        ticks: u64,
    },
    TickLimit,
}

/// The events the dialogue runner raises (spec §5.11; the compiler's built-in event types, not the host
/// API's): an instance takes them like the host's events.
pub const RUNNER_EVENTS: [&str; 3] = ["Talked", "DialogueEnded", "SceneEnded"];

/// The [`VmConfig`] of instance `instance` of `program` against the stub's API (its events and the
/// [`RUNNER_EVENTS`]).
pub(crate) fn vm_config(
    mem_bytes: u32,
    instance: u16,
    roles: Vec<(String, WireValue)>,
) -> VmConfig {
    let api = stub_api();
    let mut events: Vec<String> = api.events.into_iter().map(|e| e.name).collect();
    events.extend(RUNNER_EVENTS.iter().map(|e| e.to_string()));
    VmConfig {
        mem_bytes,
        epoch: 1,
        host_api_version: api.version,
        events,
        instance,
        roles,
    }
}

pub struct Runner {
    pub vm: Vm,
    pub host: StubHost,
    /// The tick the next `step` runs.
    pub now: u64,
}

impl Runner {
    pub fn new(
        program: Arc<Program>,
        mem_bytes: u32,
        cfg: StubConfig,
    ) -> Result<Runner, LoadError> {
        Runner::for_instance(program, mem_bytes, cfg, 0, Vec::new())
    }

    /// A runner of instance `instance` of a quest program (0 the home, `1 + i` part `i`), with the quest's
    /// role values (`VmConfig::roles`).
    pub fn for_instance(
        program: Arc<Program>,
        mem_bytes: u32,
        cfg: StubConfig,
        instance: u16,
        roles: Vec<(String, WireValue)>,
    ) -> Result<Runner, LoadError> {
        let vm = Vm::new(program, vm_config(mem_bytes, instance, roles))?;
        Ok(Runner {
            vm,
            host: StubHost::new(cfg),
            now: 0,
        })
    }

    /// One tick: deliver what scripts emitted during the previous one, answer the requests due now (in
    /// promise-id order), run the VM, advance the clock.
    pub fn step(&mut self, fuel: u64) -> TickReport {
        for event in std::mem::take(&mut self.host.emitted) {
            let name = match &event {
                WireValue::Struct { name, .. } => name.clone(),
                _ => continue,
            };
            let report = match self.vm.deliver(&name, &event) {
                Delivery::Accepted => continue,
                Delivery::Queued => Report::EventQueued { event: name },
                Delivery::Dropped(reason) => Report::EventDropped {
                    event: name,
                    reason,
                },
            };
            self.host.reports.push(report);
        }
        for (id, answer) in self.host.due(self.now) {
            self.vm.resolve(id, answer);
        }
        self.host.tick = self.now;
        let report = self.vm.tick(self.now, fuel, &mut self.host);
        self.host.expire(self.now);
        self.now += 1;
        report
    }

    /// Asks for `main`, then steps until [`Runner::quiescent`] or `max_ticks` steps ran.
    pub fn run_main(&mut self, fuel: u64, max_ticks: u64) -> RunOutcome {
        self.vm.start_main();
        for ticks in 1..=max_ticks {
            self.step(fuel);
            if self.quiescent() {
                return RunOutcome::Finished { ticks };
            }
        }
        RunOutcome::TickLimit
    }

    /// The VM is idle and the host holds nothing for it: no request in flight (every parked handler waits
    /// on one) and no undelivered emit. The VM has no public parked-handler query, so this relies on that.
    pub fn quiescent(&self) -> bool {
        self.vm.is_idle() && !self.host.has_pending() && self.host.emitted.is_empty()
    }
}
