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

#[test]
fn save_then_load_changed_program() {
    let a =
        "let runs = 0\nlet gold = 7\n\nfn main():\n    runs = runs + 1\n    print(f\"{runs}\")\n";
    let b = "let runs = 0\nlet bonus = 10\n\nfn main():\n    runs = runs + 1\n    print(f\"{runs} {bonus}\")\n";
    let d = dir("migrate", &[("main.qs", a)]);
    let file = d.join("state.bin");
    let f = file.to_str().unwrap();
    let first = quest(&["run", "--save", f], &d);
    assert_eq!(
        (code(&first), out(&first)),
        (0, "1\n".to_string()),
        "{}",
        err(&first)
    );
    std::fs::write(d.join("main.qs"), b).unwrap();
    // The migrated state (with its orphan) is saved again; the reset run loads it.
    let second = quest(&["run", "--load", f, "--save", f], &d);
    assert_eq!(
        (code(&second), out(&second)),
        (0, "2 10\n".to_string()),
        "{}",
        err(&second)
    );
    assert_eq!(
        err(&second),
        "carried: runs\ninitialised: bonus\norphaned: gold\norphan: gold: int = Int(7)\n"
    );
    let reset = quest(&["run", "--load", f, "--reset"], &d);
    assert_eq!(
        (code(&reset), out(&reset)),
        (0, "1 10\n".to_string()),
        "{}",
        err(&reset)
    );
}

// ---- quests (`World`) -----------------------------------------------------------------------------------

/// Runs `quest run <dir> args…` with `stdin` as its input.
fn quest_stdin(args: &[&str], dir: &PathBuf, stdin: &str) -> Output {
    use std::io::Write as _;
    let mut child = Command::new(env!("CARGO_BIN_EXE_quest"))
        .args(&args[..1])
        .arg(dir)
        .args(&args[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

const LOST_DOG: &str = r#"# Package errantry/lost-dog — src/main.qs

let NIGHT_TICKS = 9000                  # from finding the dog to it bolting, unless fed

quest LostDog:
    requires act() >= 1
    role farmer: Person where farmer.occupation == Occupation::Farmer and farmer.alive
    role dog: Creature where dog.has_tag("pet") and dog.owner == farmer
    optional role miller: Person where miller.occupation == Occupation::Miller and near(miller, farmer, regions=1)

    let attempts = 0                    # quest state: how often the dog was found and lost again

    event Accepted:
        player: Player
    event AskedMiller:
        player: Player
    event DogFound:
        player: Player
    event DogFed:
        player: Player
    event Nightfall:
        attempt: int
    event Reunited:
        player: Player

    # ── Open: the farmer asks anyone who talks to him ─────────────────────────
    stage Open:
        dialogue worry(p: Player, npc: Person) for farmer:          # a greeting: no topic
            node hi when once per p:
                npc: "Stranger! Have you seen a brown dog on the road?"         @lostdog.worry.first
            node hi:
                => npc: "Still no sign of him."                                 @lostdog.worry.a
                => npc: "I left the gate open. My own fault."                   @lostdog.worry.b

        dialogue plea(p: Player, npc: Person) for farmer topic f"About {dog.name}" @lostdog.topic:
            npc: f"{dog.name} ran off toward the hills two days ago."           @lostdog.plea
            npc: "He's never been gone a night before."                         @lostdog.plea2
            choice:
                "I'll find him."                                                @lostdog.accept:
                    emit Accepted(player=p)
                    npc: "Bless you. Take something to eat. He's skittish."     @lostdog.hint
                "What does he look like?"                                       @lostdog.ask_look:
                    npc: "Brown, one ear up. Answers to his name, mostly."      @lostdog.look
                "Not now."                                                      @lostdog.not_now

        @on(Accepted)
        fn accepted(e):
            join(e.player)
            -> Search

    # ── Search: two objectives in parallel, one optional ─────────────────────
    stage Search:
        objective find_dog f"Find {dog.name}"                    @lostdog.find     at dog
        objective feed_dog f"Coax {dog.name} with food"          @lostdog.feed     at dog
        optional objective ask_miller "Ask at the mill"          @lostdog.ask_mill at miller
        -> Returning when find_dog.done and feed_dog.done

        dialogue join_in(p: Player, npc: Person) for farmer topic "Can I help look?" @lostdog.join if not (p in participants):
            npc: "Every pair of eyes helps."                                    @lostdog.join_ok
            emit Accepted(player=p)

        dialogue lost_again(p: Player, npc: Person) for farmer:
            node hi when attempts >= 2:
                npc: f"Found and lost {attempts} times now. He's frightened."   @lostdog.again
            node hi:
                npc: "Any luck?"                                                @lostdog.any_luck

        dialogue tip(p: Player, npc: Person) for miller topic f"Seen {dog.name}?" @lostdog.mill_topic if p in participants:
            npc: "Something's been at my grain sacks down by the weir."         @lostdog.mill_tip
            emit AskedMiller(player=p)

        @on(Entered)
        fn count_attempt(e):
            attempts = attempts + 1

        @on(Accepted)
        fn joined_late(e):
            join(e.player)

        @on(AskedMiller)
        fn asked(e):
            ask_miller.finish()

        @on(DogFound)
        fn found(e):
            if not find_dog.done:       # two players may spot him in one tick
                find_dog.finish()
                after(NIGHT_TICKS, Nightfall(attempt=attempts))

        @on(DogFed)
        fn fed(e):
            if find_dog.done:
                feed_dog.finish()
                follow(dog, e.player)

        @on(Nightfall)
        fn bolted(e):
            -> Search                   # a failed attempt: re-entering re-opens both objectives

    # ── Returning: bring him home ────────────────────────────────────────────
    stage Returning:
        objective bring_home f"Bring {dog.name} home to {farmer.name}"   @lostdog.return at farmer

        dialogue reunion(p: Player, npc: Person) for farmer topic f"{dog.name} is back" @lostdog.back_topic if near(dog, farmer, blocks=8):
            npc: f"{dog.name}! You daft mutt."                                  @lostdog.reunion
            npc: "Take this. My father carved it. It's all I have of worth."    @lostdog.reward
            emit Reunited(player=p)

        @on(Reunited)
        fn reunited(e):
            bring_home.finish()
            for q in participants:
                give(q, Item(kind="carved_whistle", count=1))
            complete()                  # for every participant

    # ── In any stage ─────────────────────────────────────────────────────────
    @on(RoleLost)
    fn lost(e):
        match e.role:
            case Role::miller:
                pass                    # optional: the tip is simply gone
            case _:
                fail()                  # the farmer or the dog: the quest cannot be done

    # ── The part that rides with the dog ─────────────────────────────────────
    part dog_watch on dog:
        @on(PlayerNear)
        fn spotted(e):
            if stage == Stage::Search and e.player in participants and not find_dog.done:
                emit DogFound(player=e.player)

        @on(ItemGiven)
        fn offered(e):
            if stage == Stage::Search and e.item.kind == "meat":
                emit DogFed(player=e.player)
"#;

const LOST_DOG_PEOPLE: &str = r#"
fact act=1
person 3 name="Hal" occupation=Farmer pos=0,0,0
creature 7 name="Rufus" tags=pet owner=3 pos=2,0,0
person 4 name="Mira" occupation=Miller
player 1 name="Ana" inv.meat=1
player 2 name="Bo"
"#;

const LOST_DOG_ROLES: &str = "farmer=Person(3),dog=Creature(7),miller=Person(4)";

/// Ana talks to Hal: the topic, "I'll find him.", then she walks away at the hub.
const LOST_DOG_TALK: &str = "[1] Hal: Stranger! Have you seen a brown dog on the road?
[1] menu: 1) About Rufus
[2] Hal: Rufus ran off toward the hills two days ago.
[2] Hal: He's never been gone a night before.
[2] menu: 1) I'll find him. 2) What does he look like? 3) Not now.
[3] Hal: Bless you. Take something to eat. He's skittish.
[3] menu: 1) About Rufus
[4] joined Ana
[4] end Hal: Left
";

#[test]
fn run_with_roles_and_picks() {
    let d = dir(
        "picks",
        &[("main.qs", LOST_DOG), ("people.txt", LOST_DOG_PEOPLE)],
    );
    let args = [
        "run",
        "--roles",
        LOST_DOG_ROLES,
        "--talk",
        "1:3",
        "--picks",
        "1,1",
    ];
    let o = quest(&args, &d);
    assert_eq!(
        (code(&o), out(&o)),
        (0, LOST_DOG_TALK.to_string()),
        "{}",
        err(&o)
    );
    // `--lines`: the line ids, with their substitutions.
    let o = quest(&[&args[..], &["--lines"]].concat(), &d);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let lines: Vec<String> = out(&o).lines().take(3).map(String::from).collect();
    assert_eq!(
        lines,
        [
            "[1] Hal: @lostdog.worry.first",
            "[1] menu: 1) @lostdog.topic(Rufus)",
            "[2] Hal: @lostdog.plea(Rufus)",
        ]
    );
}

#[test]
fn talk_reads_stdin() {
    let d = dir(
        "talk",
        &[("main.qs", LOST_DOG), ("people.txt", LOST_DOG_PEOPLE)],
    );
    let o = quest_stdin(
        &["run", "--roles", LOST_DOG_ROLES, "--talk", "1:3"],
        &d,
        "1\nwhat\n1\nl\n",
    );
    assert_eq!(
        (code(&o), out(&o)),
        (0, LOST_DOG_TALK.to_string()),
        "{}",
        err(&o)
    );
    assert!(err(&o).contains("`what`"), "{}", err(&o));
    // The input ends: Ana walks away from the first offer.
    let o = quest_stdin(&["run", "--roles", LOST_DOG_ROLES, "--talk", "1:3"], &d, "");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).ends_with("[2] end Hal: Left\n"), "{}", out(&o));
}

#[test]
fn bad_roles_exit_2() {
    let d = dir(
        "roles",
        &[("main.qs", LOST_DOG), ("people.txt", LOST_DOG_PEOPLE)],
    );
    let wrong = "farmer=Player(1),dog=Creature(7)";
    let o = quest(&["run", "--roles", wrong], &d);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("role farmer"), "{}", err(&o));
    // A role missing, a value that does not parse.
    let o = quest(&["run", "--roles", "dog=Creature(7)"], &d);
    assert_eq!(code(&o), 2, "{}", err(&o));
    let o = quest(&["run", "--roles", "farmer=Person(3,dog=Creature(7)"], &d);
    assert_eq!(code(&o), 2, "{}", err(&o));
    // Malformed flags are usage errors.
    assert_eq!(code(&quest(&["run", "--roles", "farmer"], &d)), 64);
    assert_eq!(code(&quest(&["run", "--talk", "1-3"], &d)), 64);
    assert_eq!(code(&quest(&["run", "--picks", "1"], &d)), 64);
    let o = quest(&["run", "--talk", "1:3", "--picks", "1,0"], &d);
    assert_eq!(code(&o), 64);
}

#[test]
fn script_file_default() {
    let script = "1 talk 1 3
2 pick 1 1
3 pick 1 1
5 leave 1
6 done 1
8 near 1 7
10 event ItemGiven player=Player(1) to=Creature(7) item=Item(kind=\"meat\",count=1) @dog_watch
20 talk 1 3
21 pick 1 1
23 leave 1
24 done 1
";
    let d = dir(
        "script",
        &[
            ("main.qs", LOST_DOG),
            ("people.txt", LOST_DOG_PEOPLE),
            ("script.txt", script),
            ("fails.txt", "2 lose 4\n4 kill 7\n"),
        ],
    );
    let o = quest(&["run", "--roles", LOST_DOG_ROLES], &d);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = out(&o);
    assert!(o.starts_with("[1] Hal: Stranger!"), "{o}");
    assert!(
        o.ends_with("[22] give Ana 1 carved_whistle\n[22] completed Ana\n[23] end Hal: Left\n"),
        "{o}"
    );
    // `--script FILE` over the default; `fail()` exits 2.
    let fails = d.join("fails.txt");
    let o = quest(
        &[
            "run",
            "--roles",
            LOST_DOG_ROLES,
            "--script",
            fails.to_str().unwrap(),
        ],
        &d,
    );
    assert_eq!(
        (code(&o), out(&o)),
        (2, "[4] failed\n".to_string()),
        "{}",
        err(&o)
    );
    // A script that does not parse names its line.
    std::fs::write(d.join("script.txt"), "1 talk 1\n").unwrap();
    let o = quest(&["run", "--roles", LOST_DOG_ROLES], &d);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("script line 1"), "{}", err(&o));
}

const QUEST_WITH_MAIN: &str = r#"quest Q:
    role keeper: Person
    stage S:
        @on(Entered)
        fn s(e):
            pass

fn main():
    print("main ran")
"#;

#[test]
fn quest_main_runs_in_the_world() {
    let people = "person 3 name=\"Hal\"\n";
    let d = dir(
        "quest-main",
        &[("main.qs", QUEST_WITH_MAIN), ("people.txt", people)],
    );
    let o = quest(&["run", "--roles", "keeper=Person(3)"], &d);
    assert_eq!(
        (code(&o), out(&o)),
        (0, "[0] main ran\n".to_string()),
        "{}",
        err(&o)
    );
    // A fault in `main` is reported and exits 2, as in the runner.
    let faulting = QUEST_WITH_MAIN.replace(
        "    print(\"main ran\")",
        "    let z = 0\n    print(f\"{1 / z}\")",
    );
    std::fs::write(d.join("main.qs"), faulting).unwrap();
    let o = quest(&["run", "--roles", "keeper=Person(3)"], &d);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(
        err(&o).starts_with("error: handler main aborted: "),
        "{}",
        err(&o)
    );
}

#[test]
fn plain_program_ignores_a_stray_script_txt() {
    let d = dir(
        "stray",
        &[
            ("main.qs", SECTION_5_4),
            ("script.txt", "1 talk 1 3\n"),
            ("people.txt", "not a people file\n"),
        ],
    );
    let o = quest(&["run"], &d);
    assert_eq!(
        (code(&o), out(&o)),
        (0, "total 130, stopped at 7\n".to_string()),
        "{}",
        err(&o)
    );
}

#[test]
fn quest_save_load_reset_round_trip() {
    let d = dir(
        "quest-save",
        &[("main.qs", LOST_DOG), ("people.txt", LOST_DOG_PEOPLE)],
    );
    let file = d.join("state.bin");
    let f = file.to_str().unwrap();
    let talk = ["run", "--roles", LOST_DOG_ROLES, "--talk", "1:3"];
    let first = quest(&[&talk[..], &["--picks", "1,1", "--save", f]].concat(), &d);
    assert_eq!(code(&first), 0, "{}", err(&first));
    // Loaded: Ana joined, the quest is in `Search`.
    let o = quest(&[&talk[..], &["--picks", "l", "--load", f]].concat(), &d);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("[1] Hal: Any luck?\n"), "{}", out(&o));
    // Reset: placed again, back in `Open`.
    let o = quest(
        &[&talk[..], &["--picks", "l", "--load", f, "--reset"]].concat(),
        &d,
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("[1] Hal: Stranger!"), "{}", out(&o));
}

#[test]
fn world_input_files_and_talk_errors() {
    let d = dir(
        "world-files",
        &[
            ("main.qs", LOST_DOG),
            ("people.txt", LOST_DOG_PEOPLE),
            ("bad.txt", "dragon 9\n"),
        ],
    );
    let bad = d.join("bad.txt");
    let o = quest(
        &[
            "run",
            "--roles",
            LOST_DOG_ROLES,
            "--people",
            bad.to_str().unwrap(),
        ],
        &d,
    );
    assert_eq!(code(&o), 2, "{}", err(&o));
    let missing = d.join("missing.txt");
    let o = quest(
        &[
            "run",
            "--roles",
            LOST_DOG_ROLES,
            "--people",
            missing.to_str().unwrap(),
        ],
        &d,
    );
    assert_eq!(code(&o), 66, "{}", err(&o));
    // `--mem` reaches the world's instances: far too little does not load.
    let o = quest(&["run", "--roles", LOST_DOG_ROLES, "--mem", "64"], &d);
    assert_eq!(code(&o), 2, "{}", err(&o));
    // An unknown NPC or player: no conversation, a note, no hang.
    for pair in ["1:99", "99:3"] {
        let o = quest(
            &[
                "run",
                "--roles",
                LOST_DOG_ROLES,
                "--talk",
                pair,
                "--picks",
                "1",
            ],
            &d,
        );
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert!(err(&o).contains("started no conversation"), "{}", err(&o));
    }
}
