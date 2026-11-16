//! A generator of well-typed quest programs (plan Task 33), for the property suites: every program it
//! yields compiles against the stub's API with no diagnostic, by construction.
//!
//! A program has the top-level `g_n: int`, `g_xs: list[int]`, `g_d: dict[str, int]` and `g_s: S` (one
//! struct), one declared error set, 1–4 functions `f0 … f3` (`fi` calls only `fj` with `j < i`, so the call
//! graph is a DAG), one handler and a `main` that emits 0–3 events for it. A body works on two locals `v`
//! and `w` and the global `g_n`; its statements are drawn from: arithmetic reduced `% 1000` (so every int
//! stays below 1000 in magnitude and nothing overflows), list append / pop / index write, dict set /
//! remove, struct field writes, `if`, `while` bounded by a counter (≤ 20), a call, `raise` (functions
//! only; every call of a function that may raise is `else`-handled), `await world.add(…) else 0`, a write
//! through a reference, and `print`. Lists stay short (append is guarded by a length check), and index
//! writes are guarded, so a generated program does not fault on its own data; it may run for many ticks.

use proptest::collection::vec;
use proptest::prelude::*;

/// An int place a statement reads or writes.
#[derive(Clone, Copy, Debug)]
pub enum Var {
    /// The local `v`.
    V,
    /// The local `w`.
    W,
    /// The global `g_n`.
    G,
}

/// An operand: a place or a literal in `0..1000`.
#[derive(Clone, Copy, Debug)]
pub enum Operand {
    Var(Var),
    Lit(i64),
}

#[derive(Clone, Copy, Debug)]
pub enum ArOp {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Copy, Debug)]
pub enum Cond {
    Less(Operand, Operand),
    Eq(Operand, Operand),
    /// `a % 3 == 0`.
    Third(Operand),
}

/// What a reference statement points at.
#[derive(Clone, Copy, Debug)]
pub enum RefTarget {
    /// `&v`.
    Local,
    /// `&g_n`.
    Global,
    /// `&g_s.a`.
    Field,
    /// `&g_xs` (an append through it).
    List,
}

/// One generated statement (rendered by [`render`]).
#[derive(Clone, Debug)]
pub enum Stmt {
    /// `dst = (a op b) % 1000`.
    Arith {
        dst: Var,
        a: Operand,
        op: ArOp,
        b: Operand,
    },
    /// `if g_xs.len() < 50: g_xs.append(x)`.
    Append(Var),
    /// `let pN = g_xs.pop() else 0`, `x = (x + pN) % 1000`.
    Pop(Var),
    /// `g_xs[i] = x` when `0 <= i < len`.
    IndexWrite(Var, Var),
    /// `g_d["kK"] = x`.
    DictSet(u8, Var),
    /// `let rN = g_d.remove("kK") else 0`, `x = (x - rN) % 1000`.
    DictRemove(u8, Var),
    /// `g_s.a = x`, or (`true`) an append to `g_s.b` guarded by its length.
    FieldWrite(Var, bool),
    If(Cond, Vec<Stmt>, Vec<Stmt>),
    /// A loop of `1..=20` iterations.
    While(u8, Vec<Stmt>),
    /// `dst = f(arg)` (`else 0` when the callee may raise); the callee is picked modulo the callable ones.
    Call {
        callee: u8,
        dst: Var,
        arg: Var,
    },
    /// `if v % m == r: raise Oops | Nope` (functions only; nothing elsewhere).
    Raise {
        m: u8,
        r: u8,
        nope: bool,
    },
    /// `let aN = await world.add("kK", x) else 0`, `x = (x + aN) % 1000`.
    Await(u8, Var),
    /// A write through a reference to the target, with `x`.
    RefWrite(RefTarget, Var),
    /// `print(f"… {x}")`.
    Print(Var),
}

/// One generated program before rendering.
#[derive(Clone, Debug)]
pub struct Shape {
    /// `f0 …`: each body.
    pub fns: Vec<Vec<Stmt>>,
    pub handler: Vec<Stmt>,
    /// 0 `PlayerClicked`, 1 `PlayerEntered`, 2 `ItemDelivered`.
    pub event: u8,
    pub main: Vec<Stmt>,
    /// The player ids of the events `main` emits.
    pub emits: Vec<u16>,
}

fn var() -> impl Strategy<Value = Var> {
    prop_oneof![Just(Var::V), Just(Var::W), Just(Var::G)]
}

fn operand() -> impl Strategy<Value = Operand> {
    prop_oneof![
        3 => var().prop_map(Operand::Var),
        2 => (0i64..1000).prop_map(Operand::Lit),
    ]
}

fn cond() -> impl Strategy<Value = Cond> {
    prop_oneof![
        (operand(), operand()).prop_map(|(a, b)| Cond::Less(a, b)),
        (operand(), operand()).prop_map(|(a, b)| Cond::Eq(a, b)),
        operand().prop_map(Cond::Third),
    ]
}

fn leaf() -> impl Strategy<Value = Stmt> {
    let arith = (
        var(),
        operand(),
        prop_oneof![Just(ArOp::Add), Just(ArOp::Sub), Just(ArOp::Mul)],
        operand(),
    )
        .prop_map(|(dst, a, op, b)| Stmt::Arith { dst, a, op, b });
    let target = prop_oneof![
        Just(RefTarget::Local),
        Just(RefTarget::Global),
        Just(RefTarget::Field),
        Just(RefTarget::List),
    ];
    prop_oneof![
        4 => arith,
        1 => var().prop_map(Stmt::Append),
        1 => var().prop_map(Stmt::Pop),
        1 => (var(), var()).prop_map(|(i, x)| Stmt::IndexWrite(i, x)),
        1 => (0u8..4, var()).prop_map(|(k, x)| Stmt::DictSet(k, x)),
        1 => (0u8..4, var()).prop_map(|(k, x)| Stmt::DictRemove(k, x)),
        1 => (var(), any::<bool>()).prop_map(|(x, b)| Stmt::FieldWrite(x, b)),
        2 => (any::<u8>(), var(), var()).prop_map(|(callee, dst, arg)| Stmt::Call { callee, dst, arg }),
        1 => (2u8..9, 0u8..9, any::<bool>()).prop_map(|(m, r, nope)| Stmt::Raise { m, r: r % m, nope }),
        1 => (0u8..3, var()).prop_map(|(k, x)| Stmt::Await(k, x)),
        1 => (target, var()).prop_map(|(t, x)| Stmt::RefWrite(t, x)),
        1 => var().prop_map(Stmt::Print),
    ]
}

/// A statement, nested at most three `if` / `while` deep.
fn stmt() -> impl Strategy<Value = Stmt> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            (cond(), vec(inner.clone(), 0..4), vec(inner.clone(), 0..3))
                .prop_map(|(c, t, e)| Stmt::If(c, t, e)),
            (1u8..=20, vec(inner, 0..4)).prop_map(|(n, b)| Stmt::While(n, b)),
        ]
    })
}

fn block() -> impl Strategy<Value = Vec<Stmt>> {
    vec(stmt(), 0..6)
}

/// The shape of a generated program.
pub fn shape() -> impl Strategy<Value = Shape> {
    (
        vec(block(), 1..=4),
        block(),
        0u8..3,
        block(),
        vec(0u16..1000, 0..=3),
    )
        .prop_map(|(fns, handler, event, main, emits)| Shape {
            fns,
            handler,
            event,
            main,
            emits,
        })
}

/// Well-typed sources by construction (one file, `main.qs`).
pub fn program() -> impl Strategy<Value = String> {
    shape().prop_map(|s| render(&s))
}

fn may_raise(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Raise { .. } => true,
        Stmt::If(_, t, e) => may_raise(t) || may_raise(e),
        Stmt::While(_, b) => may_raise(b),
        _ => false,
    })
}

const EVENTS: [&str; 3] = ["PlayerClicked", "PlayerEntered", "ItemDelivered"];

/// Where a body sits: function `i`, or the handler / `main` (which may call every function).
#[derive(Clone, Copy)]
enum Body {
    Fn(usize),
    Top,
}

struct Writer<'a> {
    lines: Vec<String>,
    /// For each function: whether it may raise.
    fallible: &'a [bool],
    /// The next fresh-name number.
    fresh: u32,
    body: Body,
}

fn var_text(v: Var) -> &'static str {
    match v {
        Var::V => "v",
        Var::W => "w",
        Var::G => "g_n",
    }
}

fn operand_text(o: Operand) -> String {
    match o {
        Operand::Var(v) => var_text(v).to_string(),
        Operand::Lit(n) => n.to_string(),
    }
}

impl Writer<'_> {
    fn line(&mut self, depth: usize, text: impl AsRef<str>) {
        self.lines
            .push(format!("{}{}", "    ".repeat(depth), text.as_ref()));
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    /// The statements at `depth`; `pass` when they render to nothing.
    fn block(&mut self, depth: usize, stmts: &[Stmt]) {
        let before = self.lines.len();
        for s in stmts {
            self.stmt(depth, s);
        }
        if self.lines.len() == before {
            self.line(depth, "pass");
        }
    }

    fn stmt(&mut self, d: usize, s: &Stmt) {
        match s {
            Stmt::Arith { dst, a, op, b } => {
                let op = match op {
                    ArOp::Add => "+",
                    ArOp::Sub => "-",
                    ArOp::Mul => "*",
                };
                let (a, b) = (operand_text(*a), operand_text(*b));
                self.line(d, format!("{} = ({a} {op} {b}) % 1000", var_text(*dst)));
            }
            Stmt::Append(x) => {
                self.line(d, "if g_xs.len() < 50:");
                self.line(d + 1, format!("g_xs.append({})", var_text(*x)));
            }
            Stmt::Pop(x) => {
                let p = self.fresh("p");
                let x = var_text(*x);
                self.line(d, format!("let {p} = g_xs.pop() else 0"));
                self.line(d, format!("{x} = ({x} + {p}) % 1000"));
            }
            Stmt::IndexWrite(i, x) => {
                let i = var_text(*i);
                self.line(d, format!("if {i} >= 0:"));
                self.line(d + 1, format!("if {i} < g_xs.len():"));
                self.line(d + 2, format!("g_xs[{i}] = {}", var_text(*x)));
            }
            Stmt::DictSet(k, x) => {
                self.line(d, format!("g_d[\"k{k}\"] = {}", var_text(*x)));
            }
            Stmt::DictRemove(k, x) => {
                let r = self.fresh("r");
                let x = var_text(*x);
                self.line(d, format!("let {r} = g_d.remove(\"k{k}\") else 0"));
                self.line(d, format!("{x} = ({x} - {r}) % 1000"));
            }
            Stmt::FieldWrite(x, list) => {
                let x = var_text(*x);
                if *list {
                    self.line(d, "if g_s.b.len() < 20:");
                    self.line(d + 1, format!("g_s.b.append({x})"));
                } else {
                    self.line(d, format!("g_s.a = {x}"));
                }
            }
            Stmt::If(c, then, els) => {
                let c = match c {
                    Cond::Less(a, b) => format!("{} < {}", operand_text(*a), operand_text(*b)),
                    Cond::Eq(a, b) => format!("{} == {}", operand_text(*a), operand_text(*b)),
                    Cond::Third(a) => format!("{} % 3 == 0", operand_text(*a)),
                };
                self.line(d, format!("if {c}:"));
                self.block(d + 1, then);
                if !els.is_empty() {
                    self.line(d, "else:");
                    self.block(d + 1, els);
                }
            }
            Stmt::While(n, body) => {
                let i = self.fresh("i");
                self.line(d, format!("let {i} = 0"));
                self.line(d, format!("while {i} < {n}:"));
                self.line(d + 1, format!("{i} = {i} + 1"));
                self.block(d + 1, body);
            }
            Stmt::Call { callee, dst, arg } => {
                let callable = match self.body {
                    Body::Fn(i) => i,
                    Body::Top => self.fallible.len(),
                };
                if callable == 0 {
                    return;
                }
                let j = usize::from(*callee) % callable;
                let (dst, arg) = (var_text(*dst), var_text(*arg));
                let handled = if self.fallible.get(j).copied().unwrap_or(false) {
                    " else 0"
                } else {
                    ""
                };
                let c = self.fresh("c");
                self.line(d, format!("let {c} = f{j}({arg}){handled}"));
                self.line(d, format!("{dst} = {c} % 1000"));
            }
            Stmt::Raise { m, r, nope } => {
                if let Body::Fn(_) = self.body {
                    self.line(d, format!("if v % {m} == {r}:"));
                    self.line(d + 1, if *nope { "raise Nope" } else { "raise Oops" });
                }
            }
            Stmt::Await(k, x) => {
                let a = self.fresh("a");
                let x = var_text(*x);
                self.line(
                    d,
                    format!("let {a} = await world.add(\"n{k}\", {x}) else 0"),
                );
                self.line(d, format!("{x} = ({x} + {a}) % 1000"));
            }
            Stmt::RefWrite(t, x) => {
                let r = self.fresh("q");
                let x = var_text(*x);
                let place = match t {
                    RefTarget::Local => "v",
                    RefTarget::Global => "g_n",
                    RefTarget::Field => "g_s.a",
                    RefTarget::List => "g_xs",
                };
                self.line(d, format!("let {r} = &{place}"));
                if let RefTarget::List = t {
                    self.line(d, format!("if {r}.len() < 50:"));
                    self.line(d + 1, format!("{r}.append({x})"));
                } else {
                    self.line(d, format!("*{r} = (*{r} + {x}) % 1000"));
                }
            }
            Stmt::Print(x) => {
                let x = var_text(*x);
                self.line(d, format!("print(f\"{x}={{{x}}}\")"));
            }
        }
    }
}

/// The source text of a shape.
pub fn render(s: &Shape) -> String {
    let fallible: Vec<bool> = s.fns.iter().map(|b| may_raise(b)).collect();
    let mut w = Writer {
        lines: vec![
            "error GenError = Oops | Nope".into(),
            String::new(),
            "struct S:".into(),
            "    a: int".into(),
            "    b: list[int]".into(),
            String::new(),
            "let g_n = 0".into(),
            "let g_xs: list[int] = []".into(),
            "let g_d: dict[str, int] = {}".into(),
            "let g_s = S(a=0, b=[])".into(),
        ],
        fallible: &fallible,
        fresh: 0,
        body: Body::Top,
    };
    for (i, body) in s.fns.iter().enumerate() {
        w.body = Body::Fn(i);
        w.line(0, "");
        w.line(0, format!("fn f{i}(x: int) -> int:"));
        w.line(1, "let v = x % 1000");
        w.line(1, format!("let w = {}", i * 7));
        w.block(1, body);
        w.line(1, "return (v + w) % 1000");
    }
    w.body = Body::Top;
    let event = EVENTS[usize::from(s.event) % EVENTS.len()];
    w.line(0, "");
    w.line(0, format!("@on({event})"));
    w.line(0, "fn on_event(e):");
    w.line(1, "let v = e.player.id % 1000");
    w.line(1, "let w = 1");
    w.block(1, &s.handler);
    w.line(1, "print(f\"on_event {v} {w} {g_n}\")");
    w.line(0, "");
    w.line(0, "fn main():");
    w.line(1, "let v = 0");
    w.line(1, "let w = 2");
    w.block(1, &s.main);
    for id in &s.emits {
        let player = format!("Player(id={id}, name=\"p{id}\")");
        let e = match event {
            "ItemDelivered" => {
                format!("{event}(player={player}, item=Item(kind=\"k\", count={id}))")
            }
            _ => format!("{event}(player={player})"),
        };
        w.line(1, format!("emit({e})"));
    }
    w.line(1, "print(f\"main {v} {w} {g_n} {g_xs} {g_d} {g_s}\")");
    let mut out = w.lines.join("\n");
    out.push('\n');
    out
}

/// Cases per property: 64, or `PROPTEST_CASES` when set (the plan's Global Constraints).
pub fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}
