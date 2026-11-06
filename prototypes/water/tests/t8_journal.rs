use water_proto::Mode;
use water_proto::scenario::{NAMES, build, step_once};
use water_proto::state::{JournalMode, Undo};

#[test]
fn brick_journal_undo_is_exact_all_scenarios() {
    for name in NAMES {
        let mut sc = build(name, Mode::Bodies);
        sc.world.journal = JournalMode::Brick;
        for _ in 0..40 {
            step_once(&mut sc);
        }
        let mut logs = Vec::new();
        for _ in 0..20 {
            let h = sc.world.hash();
            let (log, _) = step_once(&mut sc);
            logs.push((h, log));
        }
        while let Some((h, log)) = logs.pop() {
            sc.world.undo(log);
            assert_eq!(sc.world.hash(), h, "{name} brick undo");
        }
    }
}

#[test]
fn brick_journal_logs_each_brick_once() {
    let mut sc = build("small_spill", Mode::Bodies);
    sc.world.journal = JournalMode::Brick;
    let (log, _) = step_once(&mut sc);
    let mut seen = std::collections::BTreeSet::new();
    for u in &log {
        if let Undo::Brick(b, _) = u {
            assert!(seen.insert(*b), "brick {b} imaged twice");
        }
        assert!(!matches!(u, Undo::Water(..) | Undo::Owner(..) | Undo::Active(..) | Undo::Touch(..)));
    }
    assert!(!seen.is_empty());
    assert_eq!(sc.world.journal_bricks() as usize, seen.len());
}
