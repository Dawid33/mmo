//! Runs the built `quest` binary on temporary script directories.

use std::path::PathBuf;
use std::process::{Command, Output};

fn dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("quest-cli-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    for (path, text) in files {
        let p = d.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    d
}

fn quest(args: &[&str], dir: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quest"))
        .args(&args[..1])
        .arg(dir)
        .args(&args[1..])
        .output()
        .unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const SECTION_5_4: &str = r#"fn reward_for(stage):
    if stage == 0:
        return 0
    elif stage < 3:
        return 10 * stage
    else:
        return 50

fn main():
    let total = 0
    for s in range(5):
        total = total + reward_for(s)
    let i = 0
    while i < 10:
        i = i + 1
        if i == 7:
            break
    print(f"total {total}, stopped at {i}")
"#;

#[test]
fn check_ok_is_silent_exit_0() {
    let d = dir("check-ok", &[("main.qs", SECTION_5_4)]);
    let o = quest(&["check"], &d);
    assert_eq!(code(&o), 0);
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "");
}

#[test]
fn check_reports_rendered_diagnostics_exit_1() {
    let d = dir("check-bad", &[("main.qs", "fn f(n):\n    return f(n)\n")]);
    let o = quest(&["check"], &d);
    assert_eq!(code(&o), 1);
    assert_eq!(
        err(&o),
        "error[recursion]: recursive call cycle: f -> f\n --> main.qs:2:12\n  |\n2 |     return f(n)\n  |            ^^^^\n"
    );
}

#[test]
fn warnings_do_not_fail() {
    // Find a warning-only program: a typo'd error name (`error-typo`).
    let d = dir("warn", &[("main.qs", WARNING_SRC)]);
    let o = quest(&["check"], &d);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(err(&o).starts_with("warning[error-typo]"), "{}", err(&o));
}

const WARNING_SRC: &str = "fn take() -> int:\n    raise QuestClsed\n\nfn other() -> int:\n    raise QuestClosed\n\nfn main():\n    let a = take() else 0\n    match other():\n        case Ok(n):\n            pass\n        case Err(QuestClosed):\n            pass\n    print(f\"{a}\")\n";

#[test]
fn run_prints_output() {
    let d = dir("run", &[("main.qs", SECTION_5_4)]);
    let o = quest(&["run"], &d);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "total 130, stopped at 7\n");
}

#[test]
fn trace_lines() {
    let d = dir("trace", &[("main.qs", SECTION_5_4)]);
    let a = quest(&["run", "--trace", "--fuel", "50"], &d);
    let b = quest(&["run", "--trace", "--fuel", "50"], &d);
    assert_eq!(code(&a), 0, "{}", err(&a));
    let stderr = err(&a);
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(lines.len() > 1);
    for (n, l) in lines.iter().enumerate() {
        let parts: Vec<&str> = l.split(' ').collect();
        assert_eq!(parts.len(), 4, "{l}");
        assert_eq!(parts[0], format!("tick={n}"));
        assert!(
            parts[1]
                .strip_prefix("fuel=")
                .unwrap()
                .parse::<u64>()
                .is_ok(),
            "{l}"
        );
        let h = parts[2].strip_prefix("hash=").unwrap();
        assert!(
            h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit()),
            "{l}"
        );
        assert!(
            ["status=idle", "status=busy", "status=failed"].contains(&parts[3]),
            "{l}"
        );
    }
    assert_eq!(err(&a), err(&b));
}

#[test]
fn tick_limit_exit_3() {
    let d = dir(
        "limit",
        &[("main.qs", "fn main():\n    while true:\n        pass\n")],
    );
    let o = quest(&["run", "--ticks", "20"], &d);
    assert_eq!(code(&o), 3, "{}", err(&o));
}

#[test]
fn abort_exit_2() {
    let d = dir(
        "abort",
        &[(
            "main.qs",
            "fn main():\n    let z = 0\n    print(f\"{1 / z}\")\n",
        )],
    );
    let o = quest(&["run"], &d);
    assert_eq!(code(&o), 2);
    let e = err(&o);
    assert!(e.starts_with("error: handler main aborted: "), "{e}");
    assert!(e.contains("\n  at main.qs:3:"), "{e}");
}

#[test]
fn save_then_load_same_program() {
    let src = "let runs = 0\n\nfn main():\n    runs = runs + 1\n    print(f\"{runs}\")\n";
    let d = dir("save", &[("main.qs", src)]);
    let file = d.join("state.bin");
    let f = file.to_str().unwrap();
    let a = quest(&["run", "--save", f], &d);
    assert_eq!((code(&a), out(&a)), (0, "1\n".to_string()), "{}", err(&a));
    let b = quest(&["run", "--load", f], &d);
    assert_eq!((code(&b), out(&b)), (0, "2\n".to_string()), "{}", err(&b));
}

#[test]
fn usage_errors_exit_64() {
    let d = dir("usage", &[("main.qs", SECTION_5_4)]);
    let bin = env!("CARGO_BIN_EXE_quest");
    let run = |args: &[&str]| Command::new(bin).args(args).output().unwrap();
    assert_eq!(code(&run(&[])), 64);
    assert_eq!(code(&run(&["frobnicate", "x"])), 64);
    assert_eq!(code(&run(&["check"])), 64);
    assert_eq!(code(&quest(&["run", "--fuel"], &d)), 64);
    assert_eq!(code(&quest(&["run", "--fuel", "x"], &d)), 64);
    assert_eq!(code(&quest(&["run", "--bogus"], &d)), 64);
    assert_eq!(code(&quest(&["run", "--fail", "nope"], &d)), 64);
    assert_eq!(code(&quest(&["run", "--chest", "oak"], &d)), 64);
}

#[test]
fn missing_dir_exit_66() {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("quest-cli-does-not-exist");
    assert_eq!(code(&quest(&["check"], &d)), 66);
    assert_eq!(code(&quest(&["run"], &d)), 66);
}

#[test]
fn no_main_exit_1() {
    let d = dir("nomain", &[("main.qs", "fn helper():\n    pass\n")]);
    let o = quest(&["run"], &d);
    assert_eq!(code(&o), 1);
    assert_eq!(err(&o), "no fn main() in main.qs\n");
}
