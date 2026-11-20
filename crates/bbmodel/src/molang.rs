//! A small Molang evaluator for keyframe values: numbers and arithmetic
//! expressions such as `math.sin(query.anim_time * 360) * 10`.
//!
//! Supported: number literals, `+ - * /`, unary `-`/`!`, comparisons
//! (`< <= > >= == !=`), `&& ||`, the ternary `a ? b : c`, `??`, parentheses,
//! `math.*` functions (trigonometry in degrees, as Molang defines it),
//! `math.pi`, `query.anim_time` / `q.anim_time`, `query.life_time`, `this`,
//! and any other `variable.*` / `query.*` / `temp.*` name, read from the
//! caller's variables (0 when unset — Blockbench's preview does the same
//! for unknown names). Statements (`;`, `return`, assignments) are not
//! supported; such a value parses as 0 with an error the caller can log.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f32),
    Var(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    Coalesce,
}

/// What an expression may read while it is evaluated.
#[derive(Default, Debug, Clone)]
pub struct Context {
    /// Seconds into the animation (`query.anim_time`).
    pub anim_time: f32,
    /// Seconds the entity has existed (`query.life_time`).
    pub life_time: f32,
    /// Any other name, lower-case, with its long prefix (`variable.`,
    /// `query.`, `temp.`); the short forms `v.` `q.` `t.` are expanded.
    pub variables: HashMap<String, f32>,
}

impl Expr {
    pub fn constant(&self) -> Option<f32> {
        match self {
            Expr::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn eval(&self, cx: &Context) -> f32 {
        match self {
            Expr::Num(n) => *n,
            Expr::Var(name) => match name.as_str() {
                "query.anim_time" => cx.anim_time,
                "query.life_time" => cx.life_time,
                "math.pi" => std::f32::consts::PI,
                _ => cx.variables.get(name).copied().unwrap_or(0.0),
            },
            Expr::Neg(a) => -a.eval(cx),
            Expr::Not(a) => truth(a.eval(cx) == 0.0),
            Expr::Bin(op, a, b) => {
                let x = a.eval(cx);
                match op {
                    Op::And => return truth(x != 0.0 && b.eval(cx) != 0.0),
                    Op::Or => return truth(x != 0.0 || b.eval(cx) != 0.0),
                    Op::Coalesce => return x,
                    _ => {}
                }
                let y = b.eval(cx);
                match op {
                    Op::Add => x + y,
                    Op::Sub => x - y,
                    Op::Mul => x * y,
                    Op::Div => {
                        if y == 0.0 {
                            0.0
                        } else {
                            x / y
                        }
                    }
                    Op::Lt => truth(x < y),
                    Op::Le => truth(x <= y),
                    Op::Gt => truth(x > y),
                    Op::Ge => truth(x >= y),
                    Op::Eq => truth(x == y),
                    Op::Ne => truth(x != y),
                    Op::And | Op::Or | Op::Coalesce => unreachable!(),
                }
            }
            Expr::Ternary(c, a, b) => {
                if c.eval(cx) != 0.0 {
                    a.eval(cx)
                } else {
                    b.eval(cx)
                }
            }
            Expr::Call(name, args) => {
                let v: Vec<f32> = args.iter().map(|a| a.eval(cx)).collect();
                let a = |i: usize| v.get(i).copied().unwrap_or(0.0);
                match name.as_str() {
                    "math.sin" => libm::sinf(a(0).to_radians()),
                    "math.cos" => libm::cosf(a(0).to_radians()),
                    "math.asin" => libm::asinf(a(0)).to_degrees(),
                    "math.acos" => libm::acosf(a(0)).to_degrees(),
                    "math.atan" => libm::atanf(a(0)).to_degrees(),
                    "math.atan2" => libm::atan2f(a(0), a(1)).to_degrees(),
                    "math.abs" => a(0).abs(),
                    "math.sqrt" => a(0).sqrt(),
                    "math.exp" => libm::expf(a(0)),
                    "math.ln" => libm::logf(a(0)),
                    "math.pow" => libm::powf(a(0), a(1)),
                    "math.floor" => a(0).floor(),
                    "math.ceil" => a(0).ceil(),
                    "math.round" => a(0).round(),
                    "math.trunc" => a(0).trunc(),
                    "math.mod" => {
                        if a(1) == 0.0 {
                            0.0
                        } else {
                            a(0) % a(1)
                        }
                    }
                    "math.min" => a(0).min(a(1)),
                    "math.max" => a(0).max(a(1)),
                    "math.clamp" => a(0).max(a(1)).min(a(2)),
                    "math.lerp" => a(0) + (a(1) - a(0)) * a(2),
                    "math.lerprotate" => {
                        let d = ((a(1) - a(0)) % 360.0 + 540.0) % 360.0 - 180.0;
                        a(0) + d * a(2)
                    }
                    "math.hermite_blend" => {
                        let t = a(0);
                        3.0 * t * t - 2.0 * t * t * t
                    }
                    "math.sign" => {
                        if a(0) > 0.0 {
                            1.0
                        } else if a(0) < 0.0 {
                            -1.0
                        } else {
                            0.0
                        }
                    }
                    // Render-only randomness: a fixed midpoint keeps poses stable.
                    "math.random" => (a(0) + a(1)) / 2.0,
                    "math.random_integer" => ((a(0) + a(1)) / 2.0).floor(),
                    "math.die_roll" | "math.die_roll_integer" => a(0) * (a(1) + a(2)) / 2.0,
                    _ => 0.0,
                }
            }
        }
    }
}

fn truth(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

/// Parse a keyframe value: a JSON number or a Molang string. Empty is 0.
pub fn parse(text: &str) -> Result<Expr, String> {
    let tokens = lex(text)?;
    if tokens.is_empty() {
        return Ok(Expr::Num(0.0));
    }
    let mut p = Parser { tokens, pos: 0 };
    let e = p.ternary()?;
    if p.pos != p.tokens.len() {
        return Err(format!("unexpected {:?} in {text:?}", p.tokens[p.pos]));
    }
    Ok(fold(e))
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f32),
    Name(String),
    Sym(&'static str),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    const SYMS: [&str; 18] = [
        "&&", "||", "??", "<=", ">=", "==", "!=", "+", "-", "*", "/", "(", ")", ",", "?", ":", "<",
        ">",
    ];
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit()
            || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let n: f32 = s[start..i]
                .parse()
                .map_err(|_| format!("bad number in {s:?}"))?;
            // Molang allows a trailing `f`.
            if i < b.len() && (b[i] == b'f' || b[i] == b'F') {
                i += 1;
            }
            out.push(Tok::Num(n));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < b.len()
                && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'.')
            {
                i += 1;
            }
            out.push(Tok::Name(expand(&s[start..i].to_ascii_lowercase())));
        } else if c == '!' && b.get(i + 1) != Some(&b'=') {
            out.push(Tok::Sym("!"));
            i += 1;
        } else if let Some(sym) = SYMS.iter().find(|sym| s[i..].starts_with(**sym)) {
            out.push(Tok::Sym(sym));
            i += sym.len();
        } else {
            return Err(format!("unsupported {c:?} in {s:?}"));
        }
    }
    Ok(out)
}

fn expand(name: &str) -> String {
    for (short, long) in [
        ("q.", "query."),
        ("v.", "variable."),
        ("t.", "temp."),
        ("c.", "context."),
    ] {
        if let Some(rest) = name.strip_prefix(short) {
            return format!("{long}{rest}");
        }
    }
    name.to_string()
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn is(&self, sym: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(s)) if *s == sym)
    }
    fn take(&mut self, sym: &str) -> bool {
        if self.is(sym) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ternary(&mut self) -> Result<Expr, String> {
        let c = self.binary(0)?;
        if self.take("?") {
            let a = self.ternary()?;
            if !self.take(":") {
                // Molang's binary conditional `a ? b` (0 when false).
                return Ok(Expr::Ternary(
                    Box::new(c),
                    Box::new(a),
                    Box::new(Expr::Num(0.0)),
                ));
            }
            let b = self.ternary()?;
            return Ok(Expr::Ternary(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            let Some(Tok::Sym(s)) = self.peek() else {
                break;
            };
            let (op, prec) = match *s {
                "??" => (Op::Coalesce, 1),
                "||" => (Op::Or, 2),
                "&&" => (Op::And, 3),
                "==" => (Op::Eq, 4),
                "!=" => (Op::Ne, 4),
                "<" => (Op::Lt, 5),
                "<=" => (Op::Le, 5),
                ">" => (Op::Gt, 5),
                ">=" => (Op::Ge, 5),
                "+" => (Op::Add, 6),
                "-" => (Op::Sub, 6),
                "*" => (Op::Mul, 7),
                "/" => (Op::Div, 7),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            let rhs = self.binary(prec + 1)?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.take("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.take("+") {
            return self.unary();
        }
        if self.take("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Name(name)) => {
                self.pos += 1;
                if self.take("(") {
                    let mut args = Vec::new();
                    if !self.take(")") {
                        loop {
                            args.push(self.ternary()?);
                            if self.take(")") {
                                break;
                            }
                            if !self.take(",") {
                                return Err(format!("expected , or ) in call to {name}"));
                            }
                        }
                    }
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            Some(Tok::Sym("(")) => {
                self.pos += 1;
                let e = self.ternary()?;
                if !self.take(")") {
                    return Err("expected )".into());
                }
                Ok(e)
            }
            other => Err(format!("unexpected {other:?}")),
        }
    }
}

/// Fold constant subtrees, so plain numbers (the common case) cost nothing.
fn fold(e: Expr) -> Expr {
    let cx = Context::default();
    let constant = |e: &Expr| e.constant().is_some();
    match e {
        Expr::Neg(a) => {
            let a = fold(*a);
            if constant(&a) {
                Expr::Num(-a.eval(&cx))
            } else {
                Expr::Neg(Box::new(a))
            }
        }
        Expr::Not(a) => {
            let a = fold(*a);
            if constant(&a) {
                Expr::Num(Expr::Not(Box::new(a)).eval(&cx))
            } else {
                Expr::Not(Box::new(a))
            }
        }
        Expr::Bin(op, a, b) => {
            let (a, b) = (fold(*a), fold(*b));
            let e = Expr::Bin(op, Box::new(a), Box::new(b));
            if let Expr::Bin(_, a, b) = &e {
                if constant(a) && constant(b) {
                    return Expr::Num(e.eval(&cx));
                }
            }
            e
        }
        Expr::Ternary(c, a, b) => {
            Expr::Ternary(Box::new(fold(*c)), Box::new(fold(*a)), Box::new(fold(*b)))
        }
        Expr::Call(name, args) => {
            let args: Vec<Expr> = args.into_iter().map(fold).collect();
            let pure = !name.starts_with("math.random") && !name.starts_with("math.die_roll");
            let e = Expr::Call(name, args);
            if let Expr::Call(_, args) = &e {
                if pure && args.iter().all(constant) {
                    return Expr::Num(e.eval(&cx));
                }
            }
            e
        }
        e => e,
    }
}
