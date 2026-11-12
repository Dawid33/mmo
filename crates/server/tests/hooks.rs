//! The coordinator's test hooks stay out of the production build: every hook item and
//! every use of one in the production loop sits behind `#[cfg(feature = "test-hooks")]`.
//! "This method does not exist" cannot be asserted from a test, so this reads the source.

const SOURCE: &str = include_str!("../src/coordinator_rt.rs");
const GATE: &str = "#[cfg(feature = \"test-hooks\")]";
const HOOKS: [&str; 5] =
    ["fn with_core", "fn inspect", "fn crash_before_claim_reply", "inspect.try_iter()", "crash_before_claim_reply.load("];

#[test]
fn production_build_has_no_hooks() {
    let lines: Vec<&str> = SOURCE.lines().collect();
    let mut ungated = Vec::new();
    let mut found = 0;
    for (i, line) in lines.iter().enumerate() {
        if !HOOKS.iter().any(|h| line.contains(h)) {
            continue;
        }
        found += 1;
        if !lines[i.saturating_sub(3)..i].iter().any(|l| l.trim() == GATE) {
            ungated.push(format!("coordinator_rt.rs:{}: {}", i + 1, line.trim()));
        }
    }
    assert!(found >= 5, "the hooks were not found in the source ({found} lines): is the check stale?");
    assert!(ungated.is_empty(), "test hooks outside {GATE}:\n{}", ungated.join("\n"));
}
