//! `quest run` in the stub's `World`: a quest's home and parts (or a program driven by `--talk`, `--script`
//! or `--people`) with the people, the roles, the script's inputs and the `--talk` driver.

use std::collections::VecDeque;
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use quest_lang::{compile, render};
use quest_stub::{
    Input, Outcome, People, StubConfig, World, parse_people, parse_script, stub_api, vm_config,
};
use quest_vm::dialogue_run::ConvState;
use quest_vm::{Status, Vm};

use crate::{
    Answer, EXIT_ABORTED, EXIT_NO_INPUT, EXIT_TICK_LIMIT, RunOpts, load_state, parse_answer,
    print_report, read_tree, role_values, state_file, write_state,
};

/// The tick `--talk` talks at.
const TALK_TICK: u64 = 1;

/// `given`, else `<dir>/<default>` when it is a file and the program has a quest (a plain program takes
/// only the files it is given).
fn input_file(given: &Option<String>, dir: &str, default: &str, quest: bool) -> Option<PathBuf> {
    match given {
        Some(f) => Some(PathBuf::from(f)),
        None if quest => Some(Path::new(dir).join(default)).filter(|p| p.is_file()),
        None => None,
    }
}

/// A file's text; unreadable is exit 66.
fn read_text(file: &Path) -> Result<String, ExitCode> {
    std::fs::read_to_string(file).map_err(|e| {
        eprintln!("quest: {}: {e}", file.display());
        ExitCode::from(EXIT_NO_INPUT)
    })
}

/// What `--talk` does next: talk at [`TALK_TICK`], answer each offer (from `--picks`, else stdin), and
/// acknowledge the end.
struct Driver {
    player: i64,
    npc: i64,
    /// `--picks`' answers left; `None`: stdin's.
    picks: Option<VecDeque<Answer>>,
    talked: bool,
    /// A conversation was seen after the talk.
    seen: bool,
    over: bool,
}

impl Driver {
    /// The input for the tick `now`, from the world as the last tick left it.
    fn input(&mut self, now: u64, w: &World) -> Option<Input> {
        let player = self.player;
        if !self.talked {
            if now < TALK_TICK {
                return None;
            }
            self.talked = true;
            return Some(Input::Talk {
                player,
                npc: self.npc,
            });
        }
        let c = w.conversation(player)?;
        match c.state() {
            ConvState::AwaitingPick if c.offer().is_some() => match self.answer() {
                Answer::Pick(option) => Some(Input::Pick { player, option }),
                Answer::Leave => Some(Input::Leave { player }),
            },
            ConvState::Ended { .. } => Some(Input::Done { player }),
            _ => None,
        }
    }

    /// The next answer: `--picks`' (leaving when they ran out), else a line of stdin (leaving at its end).
    fn answer(&mut self) -> Answer {
        if let Some(picks) = &mut self.picks {
            return picks.pop_front().unwrap_or(Answer::Leave);
        }
        let stdin = std::io::stdin();
        let prompt = stdin.is_terminal();
        let mut stdin = stdin.lock();
        loop {
            if prompt {
                eprint!("> ");
            }
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => return Answer::Leave,
                Ok(_) => {}
            }
            match parse_answer(&line) {
                Some(a) => return a,
                None => eprintln!(
                    "quest: `{}` is not an option number (from 1) or `l`",
                    line.trim()
                ),
            }
        }
    }

    /// After a tick: the talk is over once the player has no conversation (refused, or closed).
    fn settle(&mut self, w: &World) {
        if !self.talked || self.over {
            return;
        }
        if w.conversation(self.player).is_some() {
            self.seen = true;
            return;
        }
        self.over = true;
        if !self.seen {
            eprintln!(
                "quest: --talk {}:{} started no conversation",
                self.player, self.npc
            );
        }
    }
}

/// `quest run` in the world: the program's instances with the people, the roles and the script's inputs;
/// `main`, when there is one, runs once in the home.
pub(crate) fn run_world(
    dir: &str,
    opts: RunOpts,
    compiled: quest_lang::Compiled,
) -> Result<ExitCode, ExitCode> {
    let quest = compiled.program.quest.is_some();
    let people = match input_file(&opts.people, dir, "people.txt", quest) {
        Some(file) => parse_people(&read_text(&file)?).map_err(|e| {
            eprintln!("error: {}: {e}", file.display());
            ExitCode::from(EXIT_ABORTED)
        })?,
        None => People::default(),
    };
    let script = match input_file(&opts.script, dir, "script.txt", quest) {
        Some(file) => parse_script(&read_text(&file)?).map_err(|e| {
            eprintln!("error: {}: {e}", file.display());
            ExitCode::from(EXIT_ABORTED)
        })?,
        None => Vec::new(),
    };
    let roles = role_values(&opts.roles, &people)?;
    let program = Arc::new(compiled.program);
    let cfg = StubConfig {
        latency: opts.latency,
        failures: opts.failures,
        chest: opts.chest,
        ..StubConfig::default()
    };
    let placed = World::new(
        Arc::clone(&program),
        people,
        roles.clone(),
        cfg,
        opts.fuel,
        opts.mem,
    );
    let mut w = placed.map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::from(EXIT_ABORTED)
    })?;
    w.set_lines(compiled.lines);
    w.show_ids(opts.lines);
    // `edit <dir>` in a script: a directory relative to the run's.
    let base = PathBuf::from(dir);
    w.set_edit(Box::new(move |d: &str| {
        let tree = read_tree(&base.join(d))?;
        match compile(&tree, &stub_api()) {
            Ok(c) => Ok((Arc::new(c.program), c.lines)),
            Err(diags) => {
                let text: Vec<String> = diags.iter().map(|d| render(d, &tree)).collect();
                Err(text.join("\n"))
            }
        }
    }));
    if let Some(file) = &opts.load {
        let bytes = std::fs::read(file).map_err(|e| {
            eprintln!("quest: {file}: {e}");
            ExitCode::from(EXIT_NO_INPUT)
        })?;
        let cannot = |e: String| {
            eprintln!("error: cannot load {file}: {e}");
            ExitCode::from(EXIT_ABORTED)
        };
        let mut names = vec!["home".to_string()];
        if let Some(q) = &program.quest {
            names.extend(q.parts.iter().map(|p| p.name.clone()));
        }
        let cfgs = (0..w.instances())
            .map(|k| {
                let k = u16::try_from(k).unwrap_or(u16::MAX);
                vm_config(opts.mem, k, roles.clone())
            })
            .collect();
        let vms = load_state(&bytes, Arc::clone(&program), cfgs, &names).map_err(cannot)?;
        // A loaded state already had its `Setup`; a reset one is given it again (below).
        w.load_vms(vms, false).map_err(cannot)?;
    }
    if opts.reset {
        w.reset().map_err(|e| {
            eprintln!("error: cannot reset: {e}");
            ExitCode::from(EXIT_ABORTED)
        })?;
    }
    // As in the runner: a loaded state that already ran `main` runs it again.
    w.start_main();
    let mut driver = opts.talk.map(|(player, npc)| Driver {
        player,
        npc,
        picks: opts.picks.map(VecDeque::from),
        talked: false,
        seen: false,
        over: false,
    });
    let (mut next, mut failed, mut finished) = (0, false, false);
    for _ in 0..opts.ticks {
        let now = w.now();
        let k = script[next..].iter().take_while(|(t, _)| *t <= now).count();
        let mut inputs: Vec<Input> = script[next..next + k]
            .iter()
            .map(|(_, i)| i.clone())
            .collect();
        next += k;
        if let Some(d) = &mut driver {
            inputs.extend(d.input(now, &w));
        }
        let report = w.step(&inputs);
        for line in &report.lines {
            println!("{line}");
        }
        for (_, r) in &report.reports {
            failed |= print_report(r);
        }
        let statuses: Vec<Status> = (0..w.instances())
            .filter_map(|k| w.vm(u16::try_from(k).ok()?).map(Vm::status))
            .collect();
        if opts.trace {
            let status = if statuses.contains(&Status::Failed) {
                "failed"
            } else if statuses.iter().any(|s| *s != Status::Idle) {
                "busy"
            } else {
                "idle"
            };
            eprintln!(
                "tick={} fuel={} hash={:016x} status={status}",
                report.tick,
                report.fuel.iter().sum::<u64>(),
                w.hash64()
            );
        }
        failed |= statuses.contains(&Status::Failed);
        if let Some(d) = &mut driver {
            d.settle(&w);
        }
        if next == script.len() && driver.as_ref().is_none_or(|d| d.over) && w.quiescent() {
            finished = true;
            break;
        }
    }
    if let Some(file) = &opts.save {
        let vms: Vec<&Vm> = (0..w.instances())
            .filter_map(|k| w.vm(u16::try_from(k).ok()?))
            .collect();
        if let Err(e) = write_state(file, &state_file(&program, &vms)) {
            eprintln!("quest: {file}: {e}");
            return Err(ExitCode::from(EXIT_NO_INPUT));
        }
    }
    failed |= matches!(w.outcome(), Outcome::Failed | Outcome::Declined);
    Ok(if failed {
        ExitCode::from(EXIT_ABORTED)
    } else if !finished {
        eprintln!("quest: tick limit reached");
        ExitCode::from(EXIT_TICK_LIMIT)
    } else {
        ExitCode::SUCCESS
    })
}
