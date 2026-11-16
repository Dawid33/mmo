//! The `quest` command: `quest check <dir>` and `quest run <dir> [flags]` (see `quest/README.md`).

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use quest_lang::{SourceTree, compile, render};
use quest_stub::{FailureKind, FailureRule, Runner, StubConfig, stub_api};
use quest_vm::host::{AbortCause, Report};
use quest_vm::{Program, Status, Vm, VmConfig};

const EXIT_ABORTED: u8 = 2;
const EXIT_TICK_LIMIT: u8 = 3;
const EXIT_USAGE: u8 = 64;
const EXIT_NO_INPUT: u8 = 66;

const USAGE: &str = "usage:
  quest check <dir>
  quest run <dir> [--fuel N] [--ticks N] [--mem BYTES] [--trace] [--latency N]
                  [--fail KIND[@NTH][:OP]] [--chest KIND=N] [--save FILE] [--load FILE] [--reset]";

struct RunOpts {
    fuel: u64,
    ticks: u64,
    mem: u32,
    trace: bool,
    latency: u64,
    failures: Vec<FailureRule>,
    chest: Vec<(String, i64)>,
    save: Option<String>,
    load: Option<String>,
    reset: bool,
}

/// A state file of `--save` / `--load`: this magic, the program's bytes (a `u32` length and the bytes),
/// then the VM's save. The program comes along so that a later run of a changed script can migrate
/// the state: reading the old values out needs the old program.
const STATE_MAGIC: [u8; 8] = *b"QSSTATE1";

fn state_file(program: &Program, vm: &Vm) -> Vec<u8> {
    let code = program.to_bytes();
    let mut out = STATE_MAGIC.to_vec();
    // A program's bytes are far below 4 GiB.
    out.extend_from_slice(&(code.len() as u32).to_le_bytes());
    out.extend_from_slice(&code);
    out.extend_from_slice(&vm.save());
    out
}

/// The instance in a state file, for `program`: loaded as it was when the file holds `program`,
/// migrated (the report on stderr) when it holds another.
fn load_state(bytes: &[u8], program: Arc<Program>, cfg: VmConfig) -> Result<Vm, String> {
    let rest = bytes
        .strip_prefix(&STATE_MAGIC[..])
        .ok_or("not a quest state file")?;
    let (len, rest) = rest.split_first_chunk::<4>().ok_or("truncated")?;
    let len = u32::from_le_bytes(*len) as usize;
    if rest.len() < len {
        return Err("truncated".into());
    }
    let (code, save) = rest.split_at(len);
    let old = Program::from_bytes(code).map_err(|e| format!("{e:?}"))?;
    if old.content_hash() == program.content_hash() {
        return Vm::load(program, cfg, save).map_err(|e| format!("{e:?}"));
    }
    let vm = Vm::load(Arc::new(old), cfg.clone(), save).map_err(|e| format!("{e:?}"))?;
    let (vm, report) = vm.migrate(program, cfg).map_err(|e| format!("{e:?}"))?;
    let lists = [
        ("carried", &report.carried),
        ("adopted", &report.adopted),
        ("initialised", &report.initialised),
        ("orphaned", &report.orphaned),
    ];
    for (what, names) in lists {
        if !names.is_empty() {
            eprintln!("{what}: {}", names.join(", "));
        }
    }
    for o in vm.orphans() {
        eprintln!("orphan: {}: {} = {:?}", o.name, o.ty, o.value);
    }
    if report.events_dropped > 0 {
        eprintln!("events dropped: {}", report.events_dropped);
    }
    if report.handlers_cancelled > 0 {
        eprintln!("handlers cancelled: {}", report.handlers_cancelled);
    }
    Ok(vm)
}

fn usage(msg: &str) -> ExitCode {
    eprintln!("quest: {msg}\n{USAGE}");
    ExitCode::from(EXIT_USAGE)
}

fn number<T: std::str::FromStr>(flag: &str, v: Option<String>) -> Result<T, String> {
    let v = v.ok_or_else(|| format!("{flag} needs a value"))?;
    v.parse()
        .map_err(|_| format!("{flag}: `{v}` is not a valid number"))
}

fn parse_fail(spec: &str) -> Result<FailureRule, String> {
    let (head, op) = match spec.split_once(':') {
        Some((h, o)) => (h, Some(o.to_string())),
        None => (spec, None),
    };
    let (kind, nth) = match head.split_once('@') {
        Some((k, n)) => (
            k,
            n.parse::<u32>()
                .ok()
                .filter(|n| *n >= 1)
                .ok_or_else(|| format!("--fail: `{n}` is not a request number (1 or more)"))?,
        ),
        None => (head, 1),
    };
    let kind = match kind {
        "timeout" => FailureKind::Timeout,
        "lost" => FailureKind::Lost,
        "changed" => FailureKind::Changed,
        "namespace-gone" => FailureKind::NamespaceGone,
        other => {
            return Err(format!(
                "--fail: unknown kind `{other}` (timeout, lost, changed, namespace-gone)"
            ));
        }
    };
    Ok(FailureRule { op, nth, kind })
}

fn parse_run(mut args: impl Iterator<Item = String>) -> Result<RunOpts, String> {
    let mut o = RunOpts {
        fuel: 2000,
        ticks: 100_000,
        mem: 262_144,
        trace: false,
        latency: 0,
        failures: Vec::new(),
        chest: Vec::new(),
        save: None,
        load: None,
        reset: false,
    };
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--fuel" => o.fuel = number(&flag, args.next())?,
            "--ticks" => o.ticks = number(&flag, args.next())?,
            "--mem" => o.mem = number(&flag, args.next())?,
            "--latency" => o.latency = number(&flag, args.next())?,
            "--trace" => o.trace = true,
            "--reset" => o.reset = true,
            "--fail" => {
                let v = args.next().ok_or("--fail needs a value")?;
                o.failures.push(parse_fail(&v)?);
            }
            "--chest" => {
                let v = args.next().ok_or("--chest needs a value")?;
                let (kind, n) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--chest: `{v}` is not KIND=N"))?;
                let n = n
                    .parse::<i64>()
                    .map_err(|_| format!("--chest: `{n}` is not a number"))?;
                o.chest.push((kind.to_string(), n));
            }
            "--save" => o.save = Some(args.next().ok_or("--save needs a file")?),
            "--load" => o.load = Some(args.next().ok_or("--load needs a file")?),
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    Ok(o)
}

/// Every `*.qs` under `root`, paths relative to it with `/` separators.
fn read_tree(root: &Path) -> Result<SourceTree, String> {
    fn walk(root: &Path, dir: &Path, tree: &mut SourceTree) -> Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
            if path.is_dir() {
                walk(root, &path, tree)?;
            } else if path.extension().is_some_and(|e| e == "qs") {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let rel = path.strip_prefix(root).unwrap_or(&path);
                let rel: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                tree.files.insert(rel.join("/"), text);
            }
        }
        Ok(())
    }
    let mut tree = SourceTree::default();
    walk(root, root, &mut tree)?;
    Ok(tree)
}

fn compile_tree(dir: &str) -> Result<(SourceTree, quest_lang::Compiled), ExitCode> {
    let tree = match read_tree(Path::new(dir)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("quest: {e}");
            return Err(ExitCode::from(EXIT_NO_INPUT));
        }
    };
    match compile(&tree, &stub_api()) {
        Ok(c) => {
            for w in &c.warnings {
                eprintln!("{}", render(w, &tree));
            }
            Ok((tree, c))
        }
        Err(diags) => {
            for d in &diags {
                eprintln!("{}", render(d, &tree));
            }
            Err(ExitCode::from(1))
        }
    }
}

fn check(dir: &str) -> ExitCode {
    match compile_tree(dir) {
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn cause_text(cause: &AbortCause) -> String {
    let mut s = String::new();
    let locs = match cause {
        AbortCause::Fault { message, stack, .. } => {
            s.push_str(message);
            stack
        }
        AbortCause::Error {
            name, data, trace, ..
        } => {
            let _ = write!(s, "error {name}");
            if !matches!(data, quest_vm::wire::WireValue::Unit) {
                let _ = write!(s, " {data:?}");
            }
            trace
        }
    };
    for l in locs {
        let _ = write!(s, "\n  at {}:{}:{}", l.file, l.line, l.col);
    }
    s
}

/// Prints a report; true when it is a failure (exit 2).
fn print_report(r: &Report) -> bool {
    match r {
        Report::EventQueued { event } => {
            eprintln!("warning: event {event} queued");
            false
        }
        Report::EventDropped { event, reason } => {
            eprintln!("error: event {event} dropped ({reason:?})");
            false
        }
        Report::HandlerAborted { task, cause } => {
            eprintln!("error: handler {task} aborted: {}", cause_text(cause));
            true
        }
        Report::InitFailed { global, cause } => {
            eprintln!("error: initialiser {global} failed: {}", cause_text(cause));
            true
        }
    }
}

fn run(dir: &str, opts: RunOpts) -> ExitCode {
    let (_, compiled) = match compile_tree(dir) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let program_has_main = compiled.program.main_fn.is_some();
    let program = Arc::new(compiled.program);
    let cfg = StubConfig {
        latency: opts.latency,
        failures: opts.failures,
        chest: opts.chest,
    };
    let mut runner = match Runner::new(program.clone(), opts.mem, cfg.clone()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot start the program: {e:?}");
            return ExitCode::from(EXIT_ABORTED);
        }
    };
    if let Some(file) = &opts.load {
        let bytes = match std::fs::read(file) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("quest: {file}: {e}");
                return ExitCode::from(EXIT_NO_INPUT);
            }
        };
        let api = stub_api();
        let vm_cfg = VmConfig {
            mem_bytes: opts.mem,
            epoch: 1,
            host_api_version: api.version,
            events: api.events.into_iter().map(|e| e.name).collect(),
        };
        match load_state(&bytes, Arc::clone(&program), vm_cfg) {
            Ok(vm) => runner.vm = vm,
            Err(e) => {
                eprintln!("error: cannot load {file}: {e}");
                return ExitCode::from(EXIT_ABORTED);
            }
        }
    }
    if opts.reset {
        runner.vm.reset();
    }
    if program_has_main {
        // A loaded state that already ran `main` is asked to run it again; one still in the middle of it
        // just resumes.
        if !runner.vm.start_main() {
            runner.vm.restart_main();
        }
    } else {
        eprintln!("no fn main() in main.qs");
        return ExitCode::from(1);
    }
    let mut failed = false;
    let mut finished = false;
    for _ in 0..opts.ticks {
        let tick = runner.now;
        let report = runner.step(opts.fuel);
        for line in runner.host.output.drain(..) {
            println!("{line}");
        }
        for r in runner.host.reports.drain(..) {
            failed |= print_report(&r);
        }
        if opts.trace {
            let status = match report.status {
                Status::Idle => "idle",
                Status::Busy => "busy",
                Status::Failed => "failed",
            };
            eprintln!(
                "tick={tick} fuel={} hash={:016x} status={status}",
                report.fuel_used,
                runner.vm.hash64()
            );
        }
        if report.status == Status::Failed {
            failed = true;
            finished = true;
            break;
        }
        if runner.quiescent() {
            finished = true;
            break;
        }
    }
    if let Some(file) = &opts.save
        && let Err(e) = std::fs::write(file, state_file(&program, &runner.vm))
    {
        eprintln!("quest: {file}: {e}");
        return ExitCode::from(EXIT_NO_INPUT);
    }
    if failed {
        ExitCode::from(EXIT_ABORTED)
    } else if !finished {
        eprintln!("quest: tick limit reached");
        ExitCode::from(EXIT_TICK_LIMIT)
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        return usage("no command");
    };
    match cmd.as_str() {
        "check" | "run" => {
            let Some(dir) = args.next() else {
                return usage(&format!("{cmd} needs a directory"));
            };
            if cmd == "check" {
                if let Some(extra) = args.next() {
                    return usage(&format!("unexpected argument `{extra}`"));
                }
                return check(&dir);
            }
            match parse_run(args) {
                Ok(opts) => run(&dir, opts),
                Err(e) => usage(&e),
            }
        }
        other => usage(&format!("unknown command `{other}`")),
    }
}
