//! The VM's invariants (spec §11) over generated programs and the corpus's world cases.
//!
//! The input: 32 seed bytes for `quest_corpus::generate::program_from_seed`, then a fuel (`1..=5000`), a
//! latency (`0..=8`), a failure rule (on the nth `world.add`), a tick `k`, a tick `j`, a world case and a
//! flag byte. A short input reads zeros.
//!
//! **Generated program** (in a `Runner`):
//! - it compiles (the generator's contract);
//! - **fuel equivalence**: run at the fuel per tick, it ends with the output, the failed-or-not status and
//!   the script state (static slots and heap) of the unlimited run. Not `Vm::hash64`: it covers the
//!   instance's clock (`H_NOW`) and the fuel debt, which a smaller budget moves by design;
//! - **rollback bar**: checkpoint before tick `k`, run to tick `j`, roll back: `hash64()` and `mem_bytes()`
//!   are the checkpoint's; with the host put back, the same ticks give the same hashes and output;
//! - **save/load**: saved before tick `k`, loaded into a fresh instance, it goes on as the uninterrupted
//!   run: per tick the same fuel and memory, at the end the same hash, output and reports;
//! - **no leaks**: idle at the end of the unlimited run and given its initial globals again (a fresh
//!   instance's, through `install_mirror`, then a tick that reaps what the install replaced), its heap is
//!   back to the fresh instance's post-init baseline. (Not "what its globals need" measured on the run
//!   itself: a list keeps the capacity it grew to.)
//!
//! **World case** (a corpus case with a quest, people or a script, in the stub's `World`):
//! - **fuel equivalence**: at the fuel per tick (a quest with parts sometimes at `1..=16`, so the home's
//!   mirror reaches a part still running its initialisers and waits), against the unlimited run: the same
//!   output (without its tick stamps), outcome, completions, people and each instance's script state. The
//!   script's inputs are given in order, each tick's batch once the world is quiescent, not at their ticks
//!   (a budget moves when a handler's effect lands; a timed input would meet another state);
//! - **save/load**: every instance saved before tick `k` (the first tick from `k` with no instance
//!   mid-segment: the stub's `World::load_vms` takes the dialogue mirrors again) and loaded in its place,
//!   the world goes on as the uninterrupted one: the same output, outcome and people, and every instance's
//!   globals equal by value. Not `World::hash64`: `load_vms` forgets which mirror each part was sent, so
//!   the home's is sent and installed again, and an install allocates its values anew — a part's heap
//!   layout, not its values, then differs.
#![no_main]

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use libfuzzer_sys::fuzz_target;
use quest_corpus::generate::program_from_seed;
use quest_corpus::{Case, RUN, config_of, is_world, tree_of};
use quest_lang::{SourceTree, compile, render};
use quest_stub::{
    FailureKind, FailureRule, Input, People, Runner, StubConfig, StubHost, World, parse_people,
    parse_script, parse_wire, vm_config,
};
use quest_vm::consts::MEM_BYTES_DEFAULT;
use quest_vm::host::Report;
use quest_vm::layout::{H_FUEL_DEBT, H_INBOX_BASE, H_INBOX_END, H_NOW};
use quest_vm::{Program, Status, Vm};

/// Ticks the unlimited run of a generated program may take (waits on latency and timeouts).
const UNLIMITED_TICKS: u64 = 2_000;
/// The most fuel a generated program may take in all for the budgeted runs (some take tens of millions).
const TOTAL_FUEL_MAX: u64 = 2_000_000;
/// The most ticks a budgeted run may take: a smaller budget is raised to fit.
const BUDGET_TICKS_MAX: u64 = 5_000;
/// Ticks beyond `total fuel / budget` a budgeted run may take.
const TICKS_SLACK: u64 = 1_000;
/// The rollback window's start and length are drawn below these.
const K_MAX: u64 = 64;
const WINDOW_MAX: u64 = 40;
/// Ticks a world run may take.
const WORLD_TICKS: u64 = 20_000;

/// The fuzzer's input, read from bytes.
struct In {
    seed: [u8; 32],
    fuel: u64,
    latency: u64,
    rule: Option<FailureRule>,
    k: u64,
    j: u64,
    world: usize,
    tiny_fuel: bool,
}

fn read(data: &[u8]) -> In {
    let b = |i: usize| data.get(i).copied().unwrap_or(0);
    let mut seed = [0; 32];
    for (i, s) in seed.iter_mut().enumerate() {
        *s = b(i);
    }
    let fuel = 1 + u64::from(u16::from_le_bytes([b(32), b(33)])) % 5_000;
    let r = b(35);
    let kind = match r % 5 {
        1 => Some(FailureKind::Lost),
        2 => Some(FailureKind::Changed),
        3 => Some(FailureKind::Timeout),
        4 => Some(FailureKind::NamespaceGone),
        _ => None,
    };
    let rule = kind.map(|kind| FailureRule {
        op: Some("world.add".to_string()),
        nth: 1 + u32::from(r / 5) % 3,
        kind,
    });
    let k = u64::from(b(36)) % K_MAX;
    In {
        seed,
        fuel,
        latency: u64::from(b(34)) % 9,
        rule,
        k,
        j: k + 1 + u64::from(b(37)) % WINDOW_MAX,
        world: usize::from(b(38)),
        tiny_fuel: b(39) & 1 == 1,
    }
}

fn runner(program: &Arc<Program>, stub: &StubConfig) -> Runner {
    let mut r = Runner::new(Arc::clone(program), MEM_BYTES_DEFAULT, stub.clone())
        .expect("a generated program loads");
    r.vm.start_main();
    r
}

/// The end of a run.
struct End {
    output: Vec<String>,
    reports: Vec<Report>,
    failed: bool,
    /// The memory without what the budget moves ([`timeless`]).
    mem: Vec<u8>,
    fuel: u64,
    /// The ticks it took.
    ticks: u64,
}

/// The reports a fuel budget does not move: all but `EventQueued`, which says an event was delivered while
/// a task was in progress — whether one was depends on how far the budget let it run.
fn settled<'a, T: 'a>(
    reports: impl IntoIterator<Item = &'a T>,
    report: impl Fn(&T) -> &Report,
) -> Vec<&'a T> {
    reports
        .into_iter()
        .filter(|r| !matches!(report(r), Report::EventQueued { .. }))
        .collect()
}

/// Word `w` of `mem` (0 past its end).
fn word(mem: &[u8], w: u64) -> u64 {
    usize::try_from(w * 8)
        .ok()
        .and_then(|at| mem.get(at..at + 8))
        .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
}

/// The inbox allocator's used-words word, relative to the inbox's base (`alloc.rs` `A_USED`).
const INBOX_A_USED: u64 = 2;

/// `mem` with what a fuel budget moves by design zeroed, and nothing else:
/// - the header words `H_NOW` (the latest tick the instance ran) and `H_FUEL_DEBT` (what a segment overran
///   its last tick by);
/// - the inbox region, but only while its allocator holds no live block (used words 0): an event's payload
///   lives there from its delivery to its dispatch, so with none live the region is allocator bookkeeping
///   only — its LIFO free lists and freed blocks' links record the ORDER deliveries and dispatches
///   interleaved in, which the budget moves; nothing reads them but the next allocation's address.
///
/// Every other byte — the static slots and the script heap, the shadow and dirty bits, the queue, the
/// promises, the parked handlers, the timers, the commands, the trace, the counters — must be equal.
fn timeless(mem: &[u8]) -> Vec<u8> {
    let mut out = mem.to_vec();
    for w in [H_NOW, H_FUEL_DEBT] {
        let at = w as usize * 8;
        if let Some(word) = out.get_mut(at..at + 8) {
            word.fill(0);
        }
    }
    let (base, end) = (
        word(mem, u64::from(H_INBOX_BASE)),
        word(mem, u64::from(H_INBOX_END)),
    );
    if base % 8 == 0 && word(mem, base / 8 + INBOX_A_USED) == 0 {
        let (Ok(base), Ok(end)) = (usize::try_from(base), usize::try_from(end)) else {
            return out;
        };
        if let Some(inbox) = out.get_mut(base..end) {
            inbox.fill(0);
        }
    }
    out
}

/// Steps until quiescent or failed, at most `ticks` ticks; `None` when it did not get there.
fn finish(r: &mut Runner, fuel: u64, ticks: u64) -> Option<End> {
    let mut used = 0u64;
    for t in 0..ticks {
        let rep = r.step(fuel);
        used = used.saturating_add(rep.fuel_used);
        let failed = rep.status == Status::Failed;
        if failed || r.quiescent() {
            return Some(End {
                output: r.host.output.clone(),
                reports: r.host.reports.clone(),
                failed,
                mem: timeless(r.vm.mem_bytes()),
                fuel: used,
                ticks: t + 1,
            });
        }
    }
    None
}

/// Every global of `program`.
fn all_globals(program: &Program) -> Vec<u16> {
    (0..program.globals.len())
        .filter_map(|g| u16::try_from(g).ok())
        .collect()
}

/// The heap words of `vm` (idle) once every global is given its initial value again, beside the post-init
/// baseline of a fresh instance (each after a tick, which reaps the values an install replaced).
fn heap_after_reinit(program: &Arc<Program>, vm: &Vm) -> Option<(u32, u32)> {
    let cfg = || vm_config(MEM_BYTES_DEFAULT, 0, Vec::new());
    let mut host = StubHost::new(StubConfig::default());
    let mut fresh = Vm::new(Arc::clone(program), cfg()).ok()?;
    fresh.initialise(0, u64::MAX, &mut host);
    fresh.tick(0, u64::MAX, &mut host);
    let init = fresh.mirror(&all_globals(program)).ok()?;
    let mut copy = Vm::load(Arc::clone(program), cfg(), &vm.save()).ok()?;
    copy.install_mirror(&init).ok()?;
    copy.tick(0, u64::MAX, &mut host);
    (fresh.is_idle() && copy.is_idle()).then(|| (copy.heap_used_words(), fresh.heap_used_words()))
}

fn generated(i: &In) {
    let text = program_from_seed(i.seed);
    let tree = SourceTree {
        files: [("main.qs".to_string(), text.clone())].into(),
    };
    let program = match compile(&tree, &quest_stub::stub_api()) {
        Ok(c) => Arc::new(c.program),
        Err(diags) => {
            let rendered: Vec<String> = diags.iter().map(|d| render(d, &tree)).collect();
            panic!(
                "a generated program does not compile:\n{}\n{text}",
                rendered.join("\n")
            );
        }
    };
    let stub = StubConfig {
        latency: i.latency,
        failures: i.rule.clone().into_iter().collect(),
        ..StubConfig::default()
    };

    // The unlimited run, and what an idle end holds.
    let mut free = runner(&program, &stub);
    let unlimited = finish(&mut free, u64::MAX, UNLIMITED_TICKS);
    if unlimited.is_some() && free.vm.is_idle() {
        // `None` only when the copy's heap cannot also hold the initial values (a nearly full heap).
        if let Some((held, baseline)) = heap_after_reinit(&program, &free.vm) {
            assert_eq!(
                held, baseline,
                "idle and given its initial globals again, the heap holds {held} words; the post-init \
                 baseline is {baseline}\n{text}"
            );
        }
    }

    // Fuel equivalence, for a run that ends within its budget of ticks.
    let mut fuel = i.fuel;
    let mut ticks = K_MAX + WINDOW_MAX;
    if let Some(unlimited) = &unlimited
        && unlimited.fuel <= TOTAL_FUEL_MAX
    {
        fuel = fuel.max(unlimited.fuel / BUDGET_TICKS_MAX + 1);
        ticks = unlimited.fuel / fuel + TICKS_SLACK + UNLIMITED_TICKS;
        let budgeted = finish(&mut runner(&program, &stub), fuel, ticks)
            .unwrap_or_else(|| panic!("not finished at fuel {fuel} after {ticks} ticks\n{text}"));
        assert_eq!(
            budgeted.output, unlimited.output,
            "output at fuel {fuel}\n{text}"
        );
        assert_eq!(
            settled(&budgeted.reports, |r| r),
            settled(&unlimited.reports, |r| r),
            "reports at fuel {fuel}\n{text}"
        );
        assert_eq!(
            budgeted.failed, unlimited.failed,
            "failed at fuel {fuel}\n{text}"
        );
        if let Some(at) = (0..budgeted.mem.len()).find(|&b| budgeted.mem[b] != unlimited.mem[b]) {
            panic!(
                "memory at fuel {fuel}: byte {at} (word {}) differs\n{text}",
                at / 8
            );
        }
    }
    // The ticks a save and a checkpoint fall before: within the unlimited run when it ended (else a
    // prefix of the run), so neither is vacuous.
    let last = unlimited.as_ref().map_or(i.k, |u| u.ticks - 1);
    let k = i.k.min(last);
    let j = k + (i.j - i.k);

    // The rollback bar: a checkpoint before tick k, the ticks to j, rolled back.
    let mut r = runner(&program, &stub);
    for _ in 0..k {
        r.step(fuel);
    }
    let cp = r.vm.checkpoint();
    let (host, now, hash, mem) = (
        r.host.clone(),
        r.now,
        r.vm.hash64(),
        r.vm.mem_bytes().to_vec(),
    );
    let window = |r: &mut Runner| -> Vec<u64> {
        (k..j)
            .map(|_| {
                r.step(fuel);
                r.vm.hash64()
            })
            .collect()
    };
    let first = window(&mut r);
    let first_output = r.host.output.clone();
    r.vm.rollback_to(cp)
        .unwrap_or_else(|e| panic!("rollback refused: {e:?}\n{text}"));
    assert_eq!(r.vm.hash64(), hash, "the hash after rollback\n{text}");
    assert!(
        r.vm.mem_bytes() == mem.as_slice(),
        "the memory after rollback\n{text}"
    );
    (r.host, r.now) = (host, now);
    assert_eq!(window(&mut r), first, "the hashes after rollback\n{text}");
    assert_eq!(
        r.host.output, first_output,
        "the output after rollback\n{text}"
    );

    // Save before tick k and load.
    let (mut plain, mut resumed) = (runner(&program, &stub), runner(&program, &stub));
    let mut saved = false;
    for tick in 0..ticks.max(k + 1) {
        if tick == k {
            let bytes = resumed.vm.save();
            resumed.vm = Vm::load(
                Arc::clone(&program),
                vm_config(MEM_BYTES_DEFAULT, 0, Vec::new()),
                &bytes,
            )
            .unwrap_or_else(|e| panic!("a save does not load: {e:?}\n{text}"));
            saved = true;
        }
        let (a, b) = (plain.step(fuel), resumed.step(fuel));
        assert_eq!(
            a.fuel_used, b.fuel_used,
            "tick {tick}: fuel after a save at {k}\n{text}"
        );
        assert!(
            plain.vm.mem_bytes() == resumed.vm.mem_bytes(),
            "tick {tick}: memory after a save at {k}\n{text}"
        );
        if saved && (a.status == Status::Failed || plain.quiescent()) {
            break;
        }
    }
    assert!(saved, "no save at tick {k}\n{text}");
    assert_eq!(
        plain.vm.hash64(),
        resumed.vm.hash64(),
        "the hash after a save\n{text}"
    );
    assert_eq!(
        plain.host.output, resumed.host.output,
        "the output after a save\n{text}"
    );
    assert_eq!(
        plain.host.reports, resumed.host.reports,
        "the reports after a save\n{text}"
    );
}

/// A world case, ready to run.
struct WorldCase {
    case: &'static Case,
    program: Arc<Program>,
    lines: BTreeMap<String, String>,
    people: People,
    script: Vec<(u64, Input)>,
    roles: Vec<(String, quest_vm::wire::WireValue)>,
    stub: StubConfig,
    parts: usize,
}

/// The corpus's world cases that compile and place.
fn world_cases() -> &'static [WorldCase] {
    static CASES: OnceLock<Vec<WorldCase>> = OnceLock::new();
    CASES.get_or_init(|| {
        let api = quest_stub::stub_api();
        RUN.iter()
            .filter(|c| is_world(c))
            .filter_map(|case| {
                let compiled = compile(&tree_of(case), &api).ok()?;
                let people = case
                    .people
                    .map_or_else(|| Ok(People::default()), parse_people)
                    .ok()?;
                let script = case
                    .script
                    .map_or_else(|| Ok(Vec::new()), parse_script)
                    .ok()?;
                let cfg = config_of(case);
                let roles = cfg
                    .roles
                    .iter()
                    .map(|(n, w)| Some((n.clone(), parse_wire(w, &people).ok()?)))
                    .collect::<Option<Vec<_>>>()?;
                let program = Arc::new(compiled.program);
                let parts = program.quest.as_ref().map_or(0, |q| q.parts.len());
                let wc = WorldCase {
                    case,
                    program,
                    lines: compiled.lines,
                    people,
                    script,
                    roles,
                    stub: StubConfig {
                        latency: cfg.latency,
                        failures: cfg.failures.clone(),
                        chest: cfg.chest.clone(),
                        ..StubConfig::default()
                    },
                    parts,
                };
                wc.world(cfg.fuel).ok()?;
                Some(wc)
            })
            .collect()
    })
}

impl WorldCase {
    fn world(&self, fuel: u64) -> Result<World, String> {
        let mut w = World::new(
            Arc::clone(&self.program),
            self.people.clone(),
            self.roles.clone(),
            self.stub.clone(),
            fuel,
            MEM_BYTES_DEFAULT,
        )?;
        w.set_lines(self.lines.clone());
        let case = self.case;
        w.set_edit(Box::new(move |dir: &str| {
            let prefix = format!("{dir}/");
            let tree = SourceTree {
                files: case
                    .files
                    .iter()
                    .filter_map(|(p, t)| {
                        Some((p.strip_prefix(&prefix)?.to_string(), t.to_string()))
                    })
                    .collect(),
            };
            compile(&tree, &quest_stub::stub_api())
                .map(|c| (Arc::new(c.program), c.lines))
                .map_err(|_| format!("{dir} does not compile"))
        }));
        w.start_main();
        Ok(w)
    }

    fn name(&self) -> &str {
        self.case.name
    }
}

/// What a world run ends with, without its clock.
struct WorldEnd {
    output: Vec<String>,
    outcome: quest_stub::Outcome,
    completions: BTreeMap<i64, u32>,
    people: People,
    /// The instances' reports, in order.
    reports: Vec<(u16, Report)>,
    /// Each instance's memory without what the budget moves ([`timeless`]).
    mems: Vec<Vec<u8>>,
}

/// The output without the `[tick] ` stamps.
fn unstamped(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(
            |l| match l.strip_prefix('[').and_then(|r| r.split_once("] ")) {
                Some((_, rest)) => rest.to_string(),
                None => l.clone(),
            },
        )
        .collect()
}

/// Runs the world with each tick's batch of the script given once it is quiescent; `None` when it does not
/// end within [`WORLD_TICKS`].
fn gated(wc: &WorldCase, fuel: u64) -> Option<WorldEnd> {
    let mut w = wc.world(fuel).ok()?;
    let mut batches: Vec<Vec<Input>> = Vec::new();
    let mut last = None;
    for (t, input) in &wc.script {
        if last != Some(*t) {
            batches.push(Vec::new());
            last = Some(*t);
        }
        batches.last_mut()?.push(input.clone());
    }
    let mut next = batches.into_iter();
    let mut reports = Vec::new();
    for _ in 0..WORLD_TICKS {
        let mut batch = Vec::new();
        if w.quiescent() {
            match next.next() {
                Some(b) => batch = b,
                None => {
                    let mems = (0..w.instances())
                        .filter_map(|k| w.vm(u16::try_from(k).ok()?))
                        .map(|vm| timeless(vm.mem_bytes()))
                        .collect();
                    return Some(WorldEnd {
                        output: unstamped(w.output()),
                        outcome: w.outcome(),
                        completions: w.completions().clone(),
                        people: w.people().clone(),
                        reports,
                        mems,
                    });
                }
            }
        }
        reports.extend(w.step(&batch).reports);
    }
    None
}

/// The inputs of the script due at the world's tick, from `next` on.
fn due(script: &[(u64, Input)], next: &mut usize, now: u64) -> Vec<Input> {
    let k = script[*next..]
        .iter()
        .take_while(|(t, _)| *t <= now)
        .count();
    let inputs = script[*next..*next + k]
        .iter()
        .map(|(_, i)| i.clone())
        .collect();
    *next += k;
    inputs
}

fn world_case(i: &In) {
    let cases = world_cases();
    if cases.is_empty() {
        return;
    }
    let wc = &cases[i.world % cases.len()];
    let name = wc.name();

    // Fuel equivalence (the mirror reaching a part still initialising, at a tiny budget).
    let fuel = if i.tiny_fuel && wc.parts > 0 {
        1 + i.fuel % 16
    } else {
        i.fuel
    };
    if let Some(unlimited) = gated(wc, u64::MAX) {
        let budgeted =
            gated(wc, fuel).unwrap_or_else(|| panic!("{name}: not finished at fuel {fuel}"));
        // Over (completed, failed, declined, reaped), a quest's instances are run no more: each stops where
        // the budget had brought it in the tick the quest ended, so only a running world's are compared.
        let running = unlimited.outcome == quest_stub::Outcome::Running;
        let mems = if running {
            budgeted.mems.as_slice()
        } else {
            &[]
        };
        for (k, (a, b)) in mems.iter().zip(&unlimited.mems).enumerate() {
            if let Some(at) = (0..a.len()).find(|&x| a[x] != b[x]) {
                panic!(
                    "{name}: instance {k}'s memory at fuel {fuel}: byte {at} (word {}) differs",
                    at / 8
                );
            }
        }
        let (b, u) = (&budgeted, &unlimited);
        assert_eq!(b.output, u.output, "{name}: the output at fuel {fuel}");
        assert_eq!(b.outcome, u.outcome, "{name}: the outcome at fuel {fuel}");
        assert_eq!(
            b.completions, u.completions,
            "{name}: the completions at fuel {fuel}"
        );
        assert_eq!(b.people, u.people, "{name}: the people at fuel {fuel}");
        assert_eq!(
            settled(&b.reports, |r| &r.1),
            settled(&u.reports, |r| &r.1),
            "{name}: the reports at fuel {fuel}"
        );
    }

    // Save and load of every instance (as `quest run --save/--load`), at the case's own schedule: before
    // tick k, k within the uninterrupted run (its length is learnt first), so the save is never vacuous.
    let fuel = i.fuel;
    let run = |w: &mut World, next: &mut usize| -> bool {
        let now = w.now();
        w.step(&due(&wc.script, next, now));
        *next == wc.script.len() && w.quiescent()
    };
    let Ok(mut plain) = wc.world(fuel) else {
        return;
    };
    let mut next_a = 0;
    let length = (1..=WORLD_TICKS)
        .find(|_| run(&mut plain, &mut next_a))
        .unwrap_or(WORLD_TICKS);
    let k = i.k.min(length - 1);
    let (Ok(mut plain), Ok(mut resumed)) = (wc.world(fuel), wc.world(fuel)) else {
        return;
    };
    let (mut next_a, mut next_b) = (0, 0);
    let mut saved = None;
    for _ in 0..WORLD_TICKS {
        let now = plain.now();
        // `World::load_vms` takes the dialogue mirrors again: not while an instance is mid-segment.
        let busy = (0..resumed.instances())
            .filter_map(|k| resumed.vm(u16::try_from(k).ok()?))
            .any(|vm| vm.in_segment());
        if saved.is_none() && now >= k && !busy {
            saved = Some(now);
            let program = Arc::clone(resumed.program());
            let vms: Vec<Vm> = (0..resumed.instances())
                .filter_map(|k| {
                    let id = u16::try_from(k).ok()?;
                    let bytes = resumed.vm(id)?.save();
                    let cfg = vm_config(MEM_BYTES_DEFAULT, id, wc.roles.clone());
                    Some(
                        Vm::load(Arc::clone(&program), cfg, &bytes).unwrap_or_else(|e| {
                            panic!("{name}: instance {id}'s save does not load: {e:?}")
                        }),
                    )
                })
                .collect();
            // `Setup` is still owed only before the first tick.
            resumed
                .load_vms(vms, now == 0)
                .unwrap_or_else(|e| panic!("{name}: the saves do not load: {e}"));
        }
        let (a, b) = (run(&mut plain, &mut next_a), run(&mut resumed, &mut next_b));
        if saved.is_some() && a && b {
            break;
        }
    }
    let at = saved.unwrap_or_else(|| panic!("{name}: no save from tick {k} on"));
    let values = |w: &World| -> Vec<Option<quest_vm::Mirror>> {
        let all = all_globals(w.program());
        (0..w.instances())
            .filter_map(|k| w.vm(u16::try_from(k).ok()?))
            .map(|vm| vm.mirror(&all).ok())
            .collect()
    };
    assert_eq!(
        resumed.output(),
        plain.output(),
        "{name}: the output after a save before tick {at}"
    );
    assert_eq!(
        resumed.outcome(),
        plain.outcome(),
        "{name}: the outcome after a save before tick {at}"
    );
    assert_eq!(
        resumed.completions(),
        plain.completions(),
        "{name}: the completions after a save before tick {at}"
    );
    assert_eq!(
        resumed.people(),
        plain.people(),
        "{name}: the people after a save before tick {at}"
    );
    assert_eq!(
        values(&resumed),
        values(&plain),
        "{name}: the instances' globals after a save before tick {at}"
    );
    assert_eq!(
        resumed.hash64(),
        plain.hash64(),
        "{name}: the world's hash after a save before tick {at}"
    );
}

fuzz_target!(|data: &[u8]| {
    let i = read(data);
    generated(&i);
    world_case(&i);
});
