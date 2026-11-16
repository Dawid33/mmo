//! The corpus against its expectations and goldens (plan Task 32). `QUEST_BLESS=1` rewrites the goldens.

use quest_corpus::{Case, FAIL, RUN, diagnostics_of, expect_text_of, golden_of};

fn blessing() -> bool {
    std::env::var_os("QUEST_BLESS").is_some_and(|v| v == "1")
}

/// Every case whose text differs from what it should be, shown with both texts.
fn mismatches(
    cases: &[Case],
    actual: impl Fn(&Case) -> String,
    expected: impl Fn(&Case) -> String,
) {
    let mut bad = Vec::new();
    for case in cases {
        let (got, want) = (actual(case), expected(case));
        if got != want {
            bad.push(format!(
                "=== {}\n--- expected\n{want}--- got\n{got}",
                case.name
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} case(s) differ:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn run_cases_match_expect() {
    mismatches(RUN, expect_text_of, |c| c.expect.to_string());
}

#[test]
fn fail_cases_match_expect() {
    mismatches(FAIL, diagnostics_of, |c| c.expect.to_string());
}

#[test]
fn goldens_are_current() {
    if blessing() {
        let corpus = concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/run");
        for case in RUN {
            let path = format!("{corpus}/{}/golden.txt", case.name);
            std::fs::write(&path, golden_of(case)).unwrap_or_else(|e| panic!("{path}: {e}"));
        }
        return;
    }
    mismatches(RUN, golden_of, |c| {
        c.golden.unwrap_or("(no golden)\n").to_string()
    });
}

#[test]
fn every_run_case_has_a_golden() {
    // A blessing run writes them; the next build embeds them.
    if blessing() {
        return;
    }
    let missing: Vec<&str> = RUN
        .iter()
        .filter(|c| c.golden.is_none())
        .map(|c| c.name)
        .collect();
    assert!(
        missing.is_empty(),
        "no golden.txt: {missing:?} (run with QUEST_BLESS=1)"
    );
}
