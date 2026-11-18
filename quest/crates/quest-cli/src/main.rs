//! The `quest` command: `quest check <dir>` and `quest run <dir> [flags]` (see `quest/README.md`). A program
//! with a quest (or a run given `--talk`, `--script` or `--people`) runs in the stub's `World`
//! ([`world_run`]); any other in a `Runner`.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use quest_lang::{SourceTree, compile, render};
use quest_stub::{FailureKind, FailureRule, People, Runner, StubConfig, parse_wire, stub_api};
use quest_vm::host::{AbortCause, Report};
use quest_vm::wire::WireValue;
use quest_vm::{Program, Status, Vm, VmConfig};

mod world_run;

const EXIT_ABORTED: u8 = 2;
const EXIT_TICK_LIMIT: u8 = 3;
const EXIT_USAGE: u8 = 64;
const EXIT_NO_INPUT: u8 = 66;

const USAGE: &str = "usage:
  quest check <dir>
  quest run <dir> [--fuel N] [--ticks N] [--mem BYTES] [--trace] [--latency N]
                  [--fail KIND[@NTH][:OP]] [--chest KIND=N] [--save FILE] [--load FILE] [--reset]
                  [--roles NAME=WIRE,...] [--people FILE] [--script FILE] [--talk PLAYER:NPC]
                  [--picks N|l,...] [--lines]";

/// One answer to an offer: an option (1-based), or walking away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    Pick(u32),
    Leave,
}

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
    /// `--roles`: (name, wire literal), parsed against the people when the run starts.
    roles: Vec<(String, String)>,
    people: Option<String>,
    script: Option<String>,
    /// `--talk`: (player, npc).
    talk: Option<(i64, i64)>,
    /// `--picks`: the answers, in turn (else stdin's).
    picks: Option<Vec<Answer>>,
    lines: bool,
}

/// A state file of `--save` / `--load` of one instance: this magic, the program's bytes (a `u32` length and
/// the bytes), then the VM's save. The program comes along so that a later run of a changed script can
/// migrate the state: reading the old values out needs the old program.
const STATE_MAGIC: [u8; 8] = *b"QSSTATE1";
/// A state file of several instances (a quest's home and parts, in id order): this magic, the program as in
/// `QSSTATE1`, a `u32` count, then each instance's save (a `u32` length and the bytes).
const STATES_MAGIC: [u8; 8] = *b"QSSTATE2";

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    // A program's or a VM's bytes are far below 4 GiB.
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn state_file(program: &Program, vms: &[&Vm]) -> Vec<u8> {
    let code = program.to_bytes();
    if let [vm] = vms {
        let mut out = STATE_MAGIC.to_vec();
        push_bytes(&mut out, &code);
        out.extend_from_slice(&vm.save());
        return out;
    }
    let mut out = STATES_MAGIC.to_vec();
    push_bytes(&mut out, &code);
    out.extend_from_slice(&(vms.len() as u32).to_le_bytes());
    for vm in vms {
        push_bytes(&mut out, &vm.save());
    }
    out
}

/// A `u32` length and that many bytes.
fn take_bytes(bytes: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let (len, rest) = bytes.split_first_chunk::<4>().ok_or("truncated")?;
    let len = u32::from_le_bytes(*len) as usize;
    if rest.len() < len {
        return Err("truncated".into());
    }
    Ok(rest.split_at(len))
}

/// A state file's program and its instances' saves.
fn read_state(bytes: &[u8]) -> Result<(Program, Vec<&[u8]>), String> {
    let (several, rest) = match (
        bytes.strip_prefix(&STATE_MAGIC[..]),
        bytes.strip_prefix(&STATES_MAGIC[..]),
    ) {
        (Some(rest), _) => (false, rest),
        (_, Some(rest)) => (true, rest),
        _ => return Err("not a quest state file".into()),
    };
    let (code, mut rest) = take_bytes(rest)?;
    let old = Program::from_bytes(code).map_err(|e| format!("{e:?}"))?;
    if !several {
        return Ok((old, vec![rest]));
    }
    let (n, tail) = rest.split_first_chunk::<4>().ok_or("truncated")?;
    rest = tail;
    let mut saves = Vec::new();
    for _ in 0..u32::from_le_bytes(*n) {
        let (save, tail) = take_bytes(rest)?;
        saves.push(save);
        rest = tail;
    }
    if !rest.is_empty() {
        return Err("unexpected bytes after the last instance".into());
    }
    Ok((old, saves))
}

/// The instances in a state file, for `program`, one per `cfgs` entry (`names` label a migration report's
/// lines, the first unlabelled): loaded as they were when the file holds `program`, migrated (the report on
/// stderr) when it holds another.
fn load_state(
    bytes: &[u8],
    program: Arc<Program>,
    cfgs: Vec<VmConfig>,
    names: &[String],
) -> Result<Vec<Vm>, String> {
    let (old, saves) = read_state(bytes)?;
    if saves.len() != cfgs.len() {
        return Err(format!(
            "the state holds {} instances, the program has {}",
            saves.len(),
            cfgs.len()
        ));
    }
    let same = old.content_hash() == program.content_hash();
    let old = Arc::new(old);
    let mut vms = Vec::new();
    for (k, (save, cfg)) in saves.into_iter().zip(cfgs).enumerate() {
        if same {
            vms.push(Vm::load(Arc::clone(&program), cfg, save).map_err(|e| format!("{e:?}"))?);
            continue;
        }
        let vm = Vm::load(Arc::clone(&old), cfg.clone(), save).map_err(|e| format!("{e:?}"))?;
        // A refused edit leaves the state file as it is: the run stops before anything is saved.
        let (vm, report) = vm.migrate(Arc::clone(&program), cfg).map_err(|(_, e)| {
            format!("the edit is refused (the state file is unchanged): {e:?}")
        })?;
        let label = match (k, names.get(k)) {
            (1.., Some(name)) => format!("{name}: "),
            _ => String::new(),
        };
        let lists = [
            ("carried", &report.carried),
            ("adopted", &report.adopted),
            ("initialised", &report.initialised),
            ("orphaned", &report.orphaned),
        ];
        for (what, names) in lists {
            if !names.is_empty() {
                eprintln!("{label}{what}: {}", names.join(", "));
            }
        }
        for o in vm.orphans() {
            eprintln!("{label}orphan: {}: {} = {:?}", o.name, o.ty, o.value);
        }
        if report.events_dropped > 0 {
            eprintln!("{label}events dropped: {}", report.events_dropped);
        }
        if report.handlers_cancelled > 0 {
            eprintln!("{label}handlers cancelled: {}", report.handlers_cancelled);
        }
        if report.timers_dropped > 0 {
            eprintln!("{label}timers dropped: {}", report.timers_dropped);
        }
        vms.push(vm);
    }
    Ok(vms)
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

/// `text` split at the commas outside quotes and brackets.
fn split_top(text: &str) -> Vec<&str> {
    let (mut depth, mut quoted, mut escaped, mut start) = (0i32, false, false, 0);
    let mut out = Vec::new();
    for (i, c) in text.char_indices() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

fn parse_roles(v: &str) -> Result<Vec<(String, String)>, String> {
    split_top(v)
        .into_iter()
        .map(|item| match item.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() && !value.trim().is_empty() => {
                Ok((name.trim().to_string(), value.trim().to_string()))
            }
            _ => Err(format!("--roles: `{item}` is not NAME=WIRE")),
        })
        .collect()
}

/// An answer to an offer: an option number (from 1), or `l` to leave.
fn parse_answer(s: &str) -> Option<Answer> {
    match s.trim() {
        "l" => Some(Answer::Leave),
        n => n.parse::<u32>().ok().filter(|n| *n >= 1).map(Answer::Pick),
    }
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
        roles: Vec::new(),
        people: None,
        script: None,
        talk: None,
        picks: None,
        lines: false,
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
            "--roles" => {
                let v = args.next().ok_or("--roles needs a value")?;
                o.roles.extend(parse_roles(&v)?);
            }
            "--people" => o.people = Some(args.next().ok_or("--people needs a file")?),
            "--script" => o.script = Some(args.next().ok_or("--script needs a file")?),
            "--talk" => {
                let v = args.next().ok_or("--talk needs a value")?;
                let pair = v
                    .split_once(':')
                    .and_then(|(p, n)| Some((p.parse().ok()?, n.parse().ok()?)));
                o.talk = Some(pair.ok_or_else(|| format!("--talk: `{v}` is not PLAYER:NPC"))?);
            }
            "--picks" => {
                let v = args.next().ok_or("--picks needs a value")?;
                let picks = v
                    .split(',')
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        parse_answer(p).ok_or_else(|| {
                            format!("--picks: `{p}` is not an option number (from 1) or `l`")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                o.picks = Some(picks);
            }
            "--lines" => o.lines = true,
            other => return Err(format!("unknown flag `{other}`")),
        }
    }
    if o.picks.is_some() && o.talk.is_none() {
        return Err("--picks needs --talk".into());
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

/// The `--roles` values, their wire literals read against `people`; one that does not parse is exit 2.
fn role_values(
    roles: &[(String, String)],
    people: &People,
) -> Result<Vec<(String, WireValue)>, ExitCode> {
    roles
        .iter()
        .map(|(name, text)| match parse_wire(text, people) {
            Ok(v) => Ok((name.clone(), v)),
            Err(e) => {
                eprintln!("error: role {name}: {e}");
                Err(ExitCode::from(EXIT_ABORTED))
            }
        })
        .collect()
}

fn run(dir: &str, opts: RunOpts) -> ExitCode {
    let (_, compiled) = match compile_tree(dir) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let world = opts.talk.is_some() || opts.script.is_some() || opts.people.is_some();
    if compiled.program.quest.is_some() || world {
        return match world_run::run_world(dir, opts, compiled) {
            Ok(code) | Err(code) => code,
        };
    }
    let program_has_main = compiled.program.main_fn.is_some();
    let program = Arc::new(compiled.program);
    let cfg = StubConfig {
        latency: opts.latency,
        failures: opts.failures,
        chest: opts.chest,
        ..StubConfig::default()
    };
    // Only a quest has roles: any given here is refused when the program loads.
    let roles = match role_values(&opts.roles, &People::default()) {
        Ok(r) => r,
        Err(code) => return code,
    };
    let runner = Runner::for_instance(program.clone(), opts.mem, cfg.clone(), 0, roles.clone());
    let mut runner = match runner {
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
            instance: 0,
            roles,
        };
        match load_state(&bytes, Arc::clone(&program), vec![vm_cfg], &[]) {
            Ok(vms) => runner.vm = vms.into_iter().next().unwrap_or(runner.vm),
            Err(e) => {
                eprintln!("error: cannot load {file}: {e}");
                return ExitCode::from(EXIT_ABORTED);
            }
        }
    }
    if opts.reset
        && let Err(e) = runner.vm.reset()
    {
        eprintln!("error: cannot reset: {e:?}");
        return ExitCode::from(EXIT_ABORTED);
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
        && let Err(e) = std::fs::write(file, state_file(&program, &[&runner.vm]))
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
