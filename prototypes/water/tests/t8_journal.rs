use water_proto::Mode;
use water_proto::scenario::{NAMES, build, step_once};
use water_proto::state::{JournalMode, Undo};

#[test]
fn brick_journal_undo_is_exact_all_scenarios() {
    // Both modes, logging from step 0 (so step-0 edits are covered), undone in reverse.
    let mut multi_brick_steps = 0;
    for mode in [Mode::Bodies, Mode::CellOnly] {
        for name in NAMES {
            let mut sc = build(name, mode);
            sc.world.journal = JournalMode::Brick;
            let mut logs = Vec::new();
            for _ in 0..60 {
                let h = sc.world.hash();
                let (log, _) = step_once(&mut sc);
                logs.push((h, log));
            }
            while let Some((h, log)) = logs.pop() {
                if log.iter().filter(|u| matches!(u, Undo::Brick(..))).count() > 1 {
                    multi_brick_steps += 1;
                }
                sc.world.undo(log);
                assert_eq!(sc.world.hash(), h, "{name} {} brick undo", mode.name());
            }
        }
    }
    assert!(multi_brick_steps > 0, "vacuous: no undone step imaged more than one brick");
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
