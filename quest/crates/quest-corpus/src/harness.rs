//! Running a case: its config line, its compile and run against the stub host, and the texts the tests
//! compare — the output a run case's `expect.txt` holds, the golden trace, the rendered diagnostics.

use std::fmt::Write as _;
use std::hash::Hasher as _;
use std::sync::Arc;

use quest_lang::{SourceTree, compile, render};
use quest_stub::{FailureKind, FailureRule, Runner, StubConfig};
use quest_vm::Fnv64;
use quest_vm::consts::{FUEL_PER_TICK_DEFAULT, MEM_BYTES_DEFAULT};
use quest_vm::host::{AbortCause, Report, SrcLoc};
use quest_vm::wire::WireValue;
use quest_vm::{Program, Status};

use crate::Case;

/// Ticks a run case may take unless its config line says otherwise.
const TICKS_DEFAULT: u64 = 10_000;

/// How a run case is run, from the config line of its `main.qs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseConfig {
    /// Fuel per tick.
    pub fuel: u64,
    /// The most ticks the run may take.
    pub ticks: u64,
    /// The stub's request latency in ticks.
    pub latency: u64,
    /// The stub's failure injection.
    pub failures: Vec<FailureRule>,
    /// The chest's contents.
    pub chest: Vec<(String, i64)>,
}

impl Default for CaseConfig {
    fn default() -> Self {
        CaseConfig {
            fuel: FUEL_PER_TICK_DEFAULT,
            ticks: TICKS_DEFAULT,
            latency: 0,
            failures: Vec::new(),
            chest: Vec::new(),
        }
    }
}

/// The case's config: the first line of its `main.qs` when it starts with `#!`, a comment to the compiler,
/// e.g. `#! fuel=200 ticks=500 latency=2 fail=Lost@2 chest=gold=3`. Words are `key=value`; `fail` and
/// `chest` may repeat. `fail=KIND[@NTH][:OP]`: the NTH (default 1) request of OP (`world.get`, …; default
/// any) fails with KIND (`Timeout`, `Lost`, `Changed`, `NamespaceGone`). `chest=KIND=N`. Defaults: fuel
/// [`FUEL_PER_TICK_DEFAULT`], 10 000 ticks, latency 0, no failures, an empty chest.
///
/// Panics on a malformed config line: the corpus is this crate's own data, and a typo in it must not pass
/// as the defaults.
pub fn config_of(case: &Case) -> CaseConfig {
    let mut cfg = CaseConfig::default();
    let main = case
        .files
        .iter()
        .find(|(path, _)| *path == "main.qs")
        .map_or("", |(_, text)| *text);
    let Some(line) = main.lines().next().and_then(|l| l.strip_prefix("#!")) else {
        return cfg;
    };
    let bad = |word: &str| -> ! { panic!("corpus case {}: bad config word `{word}`", case.name) };
    for word in line.split_whitespace() {
        let Some((key, value)) = word.split_once('=') else {
            bad(word)
        };
        let number = || value.parse::<u64>().unwrap_or_else(|_| bad(word));
        match key {
            "fuel" => cfg.fuel = number(),
            "ticks" => cfg.ticks = number(),
            "latency" => cfg.latency = number(),
            "fail" => cfg
                .failures
                .push(failure(value).unwrap_or_else(|| bad(word))),
            "chest" => {
                let (kind, n) = value.split_once('=').unwrap_or_else(|| bad(word));
                let n = n.parse::<i64>().unwrap_or_else(|_| bad(word));
                cfg.chest.push((kind.to_string(), n));
            }
            _ => bad(word),
        }
    }
    cfg
}

/// `KIND[@NTH][:OP]`.
fn failure(spec: &str) -> Option<FailureRule> {
    let (head, op) = match spec.split_once(':') {
        Some((h, o)) => (h, Some(o.to_string())),
        None => (spec, None),
    };
    let (kind, nth) = match head.split_once('@') {
        Some((k, n)) => (k, n.parse::<u32>().ok().filter(|n| *n >= 1)?),
        None => (head, 1),
    };
    let kind = match kind {
        "Timeout" => FailureKind::Timeout,
        "Lost" => FailureKind::Lost,
        "Changed" => FailureKind::Changed,
        "NamespaceGone" => FailureKind::NamespaceGone,
        _ => return None,
    };
    Some(FailureRule { op, nth, kind })
}

/// The case's sources as a compiler input.
pub fn tree_of(case: &Case) -> SourceTree {
    SourceTree {
        files: case
            .files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect(),
    }
}

/// The rendered diagnostics of compiling the case (errors and warnings, in the compiler's order), each
/// followed by a newline and separated by an empty line; for a program that compiles, a line saying so
/// and its warnings. A `fail/` case's `expect.txt` holds this text.
pub fn diagnostics_of(case: &Case) -> String {
    let tree = tree_of(case);
    let (head, diags) = match compile(&tree, &quest_stub::stub_api()) {
        Ok(c) => ("compiled without errors\n", c.warnings),
        Err(d) => ("", d),
    };
    let rendered: Vec<String> = diags.iter().map(|d| render(d, &tree) + "\n").collect();
    format!("{head}{}", rendered.join("\n"))
}

/// What one run of a case produced: the golden trace's parts ([`Trace::golden`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    /// FNV-1a 64 of `Program::to_bytes()`.
    pub bytecode: u64,
    /// Per tick: the tick, the fuel it used, the state hash after it.
    pub ticks: Vec<(u64, u64, u64)>,
    /// FNV-1a 64 of the final memory.
    pub mem: u64,
    pub output: Vec<String>,
    /// One line per report ([`report_line`]).
    pub reports: Vec<String>,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Fnv64::default();
    h.write(bytes);
    h.finish()
}

/// Runs the case as `quest run` does, under `cfg` (the case's own is [`config_of`]): the initialisers and
/// `main` are asked for at once, then ticks run until the instance is quiescent or failed, or `cfg.ticks`
/// ticks ran. `Err` is the text that stands in for the trace when the case does not compile or load.
pub fn trace_of(case: &Case, cfg: &CaseConfig) -> Result<Trace, String> {
    let tree = tree_of(case);
    let program: Arc<Program> = match compile(&tree, &quest_stub::stub_api()) {
        Ok(c) => Arc::new(c.program),
        Err(diags) => {
            let rendered: Vec<String> = diags.iter().map(|d| render(d, &tree)).collect();
            return Err(format!("compile failed:\n{}\n", rendered.join("\n")));
        }
    };
    let bytecode = fnv(&program.to_bytes());
    let stub = StubConfig {
        latency: cfg.latency,
        failures: cfg.failures.clone(),
        chest: cfg.chest.clone(),
    };
    let mut runner = Runner::new(program, MEM_BYTES_DEFAULT, stub)
        .map_err(|e| format!("load failed: {e:?}\n"))?;
    runner.vm.start_main();
    let mut ticks = Vec::new();
    for _ in 0..cfg.ticks {
        let tick = runner.now;
        let report = runner.step(cfg.fuel);
        ticks.push((tick, report.fuel_used, runner.vm.hash64()));
        if report.status == Status::Failed || runner.quiescent() {
            break;
        }
    }
    Ok(Trace {
        bytecode,
        ticks,
        mem: fnv(runner.vm.mem_bytes()),
        output: std::mem::take(&mut runner.host.output),
        reports: runner.host.reports.iter().map(report_line).collect(),
    })
}

/// [`trace_of`] under the case's own config.
fn run(case: &Case) -> Result<Trace, String> {
    trace_of(case, &config_of(case))
}

fn locs(out: &mut String, locs: &[SrcLoc]) {
    if !locs.is_empty() {
        out.push_str(" at");
    }
    for l in locs {
        let _ = write!(out, " {}:{}:{}", l.file, l.line, l.col);
    }
}

fn cause_line(out: &mut String, cause: &AbortCause) {
    match cause {
        AbortCause::Error {
            name,
            data,
            trace,
            trace_dropped,
        } => {
            let _ = write!(out, "error {name}");
            match data {
                // The error's own variant: its fields are the data.
                WireValue::Variant { fields, .. } => {
                    if !fields.is_empty() {
                        let fields: Vec<String> =
                            fields.iter().map(|(n, v)| format!("{n}={v:?}")).collect();
                        let _ = write!(out, "({})", fields.join(", "));
                    }
                }
                WireValue::Unit => {}
                other => {
                    let _ = write!(out, " {other:?}");
                }
            }
            locs(out, trace);
            if *trace_dropped > 0 {
                let _ = write!(out, " (+{trace_dropped} dropped)");
            }
        }
        AbortCause::Fault {
            kind,
            message,
            stack,
        } => {
            let _ = write!(out, "fault {kind:?} ({message})");
            locs(out, stack);
        }
    }
}

/// One report as one line: `queued <event>`, `dropped <event> <reason>`, `aborted <task>: <cause>` or
/// `init-failed <global>: <cause>`, a cause being `error <name>[(<field>=<value>, …)][ at <file:line:col>…]` or
/// `fault <kind> (<message>)[ at <file:line:col>…]`.
pub fn report_line(r: &Report) -> String {
    let mut out = String::new();
    match r {
        Report::EventQueued { event } => {
            let _ = write!(out, "queued {event}");
        }
        Report::EventDropped { event, reason } => {
            let _ = write!(out, "dropped {event} {reason:?}");
        }
        Report::HandlerAborted { task, cause } => {
            let _ = write!(out, "aborted {task}: ");
            cause_line(&mut out, cause);
        }
        Report::InitFailed { global, cause } => {
            let _ = write!(out, "init-failed {global}: ");
            cause_line(&mut out, cause);
        }
    }
    out
}

/// The output lines, then — when there are any — `reports:` and one line per report.
fn output_text(out: &mut String, run: &Trace, always_reports: bool) {
    for line in &run.output {
        out.push_str(line);
        out.push('\n');
    }
    if always_reports || !run.reports.is_empty() {
        out.push_str("reports:\n");
    }
    for r in &run.reports {
        out.push_str(r);
        out.push('\n');
    }
}

/// What a run case's `expect.txt` holds: one line per line of output, then — only when the run reported
/// something — a line `reports:` and one line per report (the golden's report lines).
pub fn expect_text_of(case: &Case) -> String {
    match run(case) {
        Ok(r) => {
            let mut out = String::new();
            output_text(&mut out, &r, false);
            out
        }
        Err(e) => e,
    }
}

/// The golden text of a run case: [`Trace::golden`] of its run under its own config, or the text that
/// stands in for it.
pub fn golden_of(case: &Case) -> String {
    match run(case) {
        Ok(r) => r.golden(),
        Err(e) => e,
    }
}

impl Trace {
    /// The golden text: `bytecode <fnv64 of Program::to_bytes(), 16 hex>`, one
    /// `tick <n> fuel <used> hash <16 hex>` per tick, `mem <fnv64 of the final memory bytes>`, `output:` and
    /// the output lines, `reports:` and one line per report.
    pub fn golden(&self) -> String {
        let mut out = format!("bytecode {:016x}\n", self.bytecode);
        for (tick, fuel, hash) in &self.ticks {
            let _ = writeln!(out, "tick {tick} fuel {fuel} hash {hash:016x}");
        }
        let _ = writeln!(out, "mem {:016x}", self.mem);
        out.push_str("output:\n");
        output_text(&mut out, self, true);
        out
    }
}
