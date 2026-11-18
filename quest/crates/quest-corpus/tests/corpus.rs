//! The corpus against its expectations and goldens (plan Task 32). `QUEST_BLESS=1` rewrites the goldens.

use quest_corpus::{
    Case, FAIL, RUN, config_of, diagnostics_of, expect_text_of, golden_of, trace_of_tree, tree_of,
};

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

/// Every ordering of `items`.
fn permutations(items: &[String]) -> Vec<Vec<String>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for (i, first) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(i);
        for mut tail in permutations(&rest) {
            tail.insert(0, first.clone());
            out.push(tail);
        }
    }
    out
}

#[test]
fn commutative_case_is_independent_of_module_order() {
    // Spec §5.11: what a player gets depends only on the set of attached dialogues, never on module order.
    let case = RUN
        .iter()
        .find(|c| c.name == "d_commutative")
        .expect("the case d_commutative");
    let golden = golden_of(case);
    let tree = tree_of(case);
    let main = tree.files.get("main.qs").expect("main.qs").clone();
    let lines: Vec<&str> = main.lines().collect();
    let at: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with("mod "))
        .collect();
    let mods: Vec<String> = at.iter().map(|&i| lines[i].to_string()).collect();
    assert_eq!(mods.len(), 3, "three modules");
    let orders = permutations(&mods);
    assert_eq!(orders.len(), 6);
    for order in orders {
        let mut text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        for (&i, m) in at.iter().zip(&order) {
            text[i] = m.clone();
        }
        let mut permuted = tree.clone();
        permuted
            .files
            .insert("main.qs".to_string(), text.join("\n") + "\n");
        let got = trace_of_tree(case, &permuted, &config_of(case))
            .expect("compiles and places")
            .golden();
        assert_eq!(got, golden, "module order {order:?}");
    }
}
