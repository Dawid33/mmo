use quest_lang::{parse_expr_str, parse_pattern_str, parse_type_str};

fn expr(text: &str) -> String {
    match parse_expr_str(text) {
        Ok(dump) => dump,
        Err(diags) => panic!("{text:?} failed: {diags:?}"),
    }
}

fn ty(text: &str) -> String {
    match parse_type_str(text) {
        Ok(dump) => dump,
        Err(diags) => panic!("{text:?} failed: {diags:?}"),
    }
}

fn pat(text: &str) -> String {
    match parse_pattern_str(text) {
        Ok(dump) => dump,
        Err(diags) => panic!("{text:?} failed: {diags:?}"),
    }
}

/// The diagnostic codes of a failing expression.
fn expr_codes(text: &str) -> Vec<&'static str> {
    match parse_expr_str(text) {
        Ok(dump) => panic!("{text:?} parsed as {dump}"),
        Err(diags) => diags.iter().map(|d| d.code).collect(),
    }
}

#[test]
fn precedence() {
    assert_eq!(expr("1 + 2 * 3"), "(+ 1 (* 2 3))");
    assert_eq!(expr("not a and b or c"), "(or (and (not a) b) c)");
    assert_eq!(expr("-a * b"), "(* (neg a) b)");
    assert_eq!(expr("(1 + 2) * 3"), "(* (+ 1 2) 3)");
    assert_eq!(expr("a - b - c"), "(- (- a b) c)");
    assert_eq!(expr("not a == b"), "(not (== a b))");
    assert_eq!(expr("a * *r"), "(* a (deref r))");
}

#[test]
fn comparisons() {
    assert_eq!(expr_codes("a < b < c"), vec!["chained-comparison"]);
    assert_eq!(expr_codes("a < b < c < d"), vec!["chained-comparison"]);
    assert_eq!(expr("x in xs"), "(in x xs)");
    assert_eq!(expr("x not in xs"), "(not-in x xs)");
    let diags = parse_expr_str("a < b == c").unwrap_err();
    assert_eq!(
        (diags[0].code, diags[0].line, diags[0].col),
        ("chained-comparison", 1, 7)
    );
}

#[test]
fn postfix_and_try() {
    assert_eq!(expr("f(x)?.y[0]"), "(index (field (? (call f x)) y) 0)");
}

#[test]
fn await_and_else() {
    assert_eq!(expr("await p else 0"), "(else (await p) 0)");
    assert_eq!(
        expr("await world.get(k)?"),
        "(? (await (call (field world get) k)))"
    );
    assert_eq!(
        expr("take(c, 20) else return"),
        "(else (call take c 20) (return))"
    );
}

#[test]
fn references() {
    assert_eq!(expr("&a.b[2]"), "(ref (index (field a b) 2))");
    assert_eq!(expr("*r"), "(deref r)");
}

#[test]
fn construction_and_collections() {
    assert_eq!(
        expr(r#"Ledger(owner="ana", entries=[])"#),
        r#"(call Ledger owner="ana" entries=(list))"#
    );
    assert_eq!(
        expr("Stage::Accepted(tick=5)"),
        "(call Stage::Accepted tick=5)"
    );
    assert_eq!(expr(r#"{"a": 1, "b": 2}"#), r#"(dict ("a" 1) ("b" 2))"#);
    assert_eq!(expr("[1, 2][0:1]"), "(slice (list 1 2) 0 1)");
    assert_eq!(expr("x[:b]"), "(slice x _ b)");
    assert_eq!(expr("x[a:]"), "(slice x a _)");
}

#[test]
fn fstring() {
    assert_eq!(
        expr(r#"f"total {total}, stopped at {i}""#),
        r#"(fstr "total " total ", stopped at " i)"#
    );
}

#[test]
fn fstring_error_is_reported_inside() {
    let diags = parse_expr_str(r#"f"ab {x +}""#).unwrap_err();
    assert_eq!(diags.len(), 1);
    assert_eq!(
        (diags[0].code, diags[0].line, diags[0].col),
        ("syntax", 1, 10)
    );
}

#[test]
fn types() {
    assert_eq!(
        ty("list[dict[str, Option[int]]]"),
        "(list (dict str (Option int)))"
    );
    assert_eq!(ty("()"), "()");
    assert_eq!(ty("npc::dialogue::Line"), "npc::dialogue::Line");
    assert_eq!(ty("Promise[int]"), "(Promise int)");
}

#[test]
fn patterns() {
    assert_eq!(pat("_"), "_");
    assert_eq!(pat("5"), "5");
    assert_eq!(pat(r#""a""#), r#""a""#);
    assert_eq!(pat("x"), "x");
    assert_eq!(pat("Stage::Accepted(tick)"), "(Stage::Accepted tick)");
    assert_eq!(pat("Stage::Accepted(tick=t)"), "(Stage::Accepted tick=t)");
    assert_eq!(
        pat("Err(ChestEmpty | QuestClosed)"),
        "(Err (| ChestEmpty QuestClosed))"
    );
    assert_eq!(
        pat("Ledger(owner=o, entries=_)"),
        "(Ledger owner=o entries=_)"
    );
    assert_eq!(pat("Some(Stage::Done)"), "(Some Stage::Done)");
}

#[test]
fn syntax_errors_name_the_expected_tokens() {
    let diags = parse_expr_str("f(x").unwrap_err();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, "syntax");
    assert!(diags[0].message.contains("`)`"), "{}", diags[0].message);
    // A found operator that cannot follow is not "expected an operator".
    let diags = parse_expr_str("a.b::c").unwrap_err();
    assert!(
        diags[0].message.starts_with("`::` cannot follow here"),
        "{}",
        diags[0].message
    );
    let diags = parse_expr_str("1 +").unwrap_err();
    assert!(
        diags[0].message.contains("an expression"),
        "{}",
        diags[0].message
    );
}

fn assert_one_too_deep(text: &str) {
    let diags = parse_expr_str(text).unwrap_err();
    let codes: Vec<_> = diags.iter().map(|d| d.code).collect();
    assert_eq!(codes, vec!["too-deep"]);
}

#[test]
fn deep_nesting_is_a_diagnostic() {
    let n = 10_000;
    assert_one_too_deep(&format!("{}1{}", "(".repeat(n), ")".repeat(n)));
    assert_one_too_deep(&format!("{}1{}", "[".repeat(n), "]".repeat(n)));
    assert_one_too_deep(&format!("{}1", "not ".repeat(n)));
    let mut sum = String::from("1");
    for _ in 1..100_000 {
        sum.push_str(" + 1");
    }
    assert_one_too_deep(&sum);
}

#[test]
fn depth_limit_is_exact() {
    let nest = |n: usize| format!("{}1{}", "(".repeat(n), ")".repeat(n));
    assert_eq!(expr(&nest(255)), "1");
    assert_one_too_deep(&nest(257));
}
