//! The s-expression dump of the tree: a test aid (`(+ 1 (* 2 3))`, `(fn main () …)`).

use super::*;
use std::fmt::Write;

fn path_text(path: &Path) -> String {
    path.join("::")
}

impl Expr {
    /// `(+ 1 (* 2 3))`, `(call f x)`, … — the form the parser tests compare.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        let op = |out: &mut String, name: &str, children: &[&Expr]| {
            out.push('(');
            out.push_str(name);
            for c in children {
                out.push(' ');
                c.dump_into(out);
            }
            out.push(')');
        };
        match &self.kind {
            ExprKind::Error => out.push_str("(error)"),
            ExprKind::Unit => out.push_str("()"),
            ExprKind::Int(i) => {
                let _ = write!(out, "{i}");
            }
            ExprKind::Float(f) => {
                let _ = write!(out, "{f:?}");
            }
            ExprKind::Bool(b) => {
                let _ = write!(out, "{b}");
            }
            ExprKind::Str(s) => {
                let _ = write!(out, "{s:?}");
            }
            ExprKind::Path(p) => out.push_str(&path_text(p)),
            ExprKind::FStr(parts) => {
                out.push_str("(fstr");
                for part in parts {
                    out.push(' ');
                    match part {
                        FStrPart::Text(t) => {
                            let _ = write!(out, "{t:?}");
                        }
                        FStrPart::Expr(e) => e.dump_into(out),
                    }
                }
                out.push(')');
            }
            ExprKind::List(items) => op(out, "list", &items.iter().collect::<Vec<_>>()),
            ExprKind::Dict(entries) => {
                out.push_str("(dict");
                for (k, v) in entries {
                    out.push_str(" (");
                    k.dump_into(out);
                    out.push(' ');
                    v.dump_into(out);
                    out.push(')');
                }
                out.push(')');
            }
            ExprKind::Unary(u, e) => {
                let name = match u {
                    UnOp::Neg => "neg",
                    UnOp::Not => "not",
                    UnOp::Ref => "ref",
                    UnOp::Deref => "deref",
                };
                op(out, name, &[e]);
            }
            ExprKind::Binary(b, l, r) => {
                let name = match b {
                    BinOp::Or => "or",
                    BinOp::And => "and",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::In => "in",
                    BinOp::NotIn => "not-in",
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Rem => "%",
                };
                op(out, name, &[l, r]);
            }
            ExprKind::Await(e) => op(out, "await", &[e]),
            ExprKind::Try(e) => op(out, "?", &[e]),
            ExprKind::Call(callee, args) => {
                out.push_str("(call ");
                callee.dump_into(out);
                for a in args {
                    out.push(' ');
                    if let Some(name) = &a.name {
                        out.push_str(name);
                        out.push('=');
                    }
                    a.value.dump_into(out);
                }
                out.push(')');
            }
            ExprKind::Field(e, name) => {
                out.push_str("(field ");
                e.dump_into(out);
                out.push(' ');
                out.push_str(name);
                out.push(')');
            }
            ExprKind::Index(e, i) => op(out, "index", &[e, i]),
            ExprKind::Slice(e, a, b) => {
                out.push_str("(slice ");
                e.dump_into(out);
                for bound in [a, b] {
                    out.push(' ');
                    match bound {
                        Some(x) => x.dump_into(out),
                        None => out.push('_'),
                    }
                }
                out.push(')');
            }
            ExprKind::Else(e, f) => op(out, "else", &[e, f]),
            ExprKind::Return(None) => out.push_str("(return)"),
            ExprKind::Return(Some(e)) => op(out, "return", &[e]),
            ExprKind::Break => out.push_str("(break)"),
            ExprKind::Continue => out.push_str("(continue)"),
            ExprKind::Raise(e) => op(out, "raise", &[e]),
            ExprKind::FallbackBlock(binding, body) => {
                let _ = write!(out, "(fallback {binding} ");
                dump_block(out, body);
                out.push(')');
            }
        }
    }
}

impl Type {
    /// `(list (dict str int))`, `()`, `npc::Line`.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        match &self.kind {
            TypeKind::Unit => "()".to_string(),
            TypeKind::Ref(t) => format!("(& {})", t.dump()),
            TypeKind::Named(path, args) if args.is_empty() => path_text(path),
            TypeKind::Named(path, args) => {
                let mut out = format!("({}", path_text(path));
                for a in args {
                    out.push(' ');
                    out.push_str(&a.dump());
                }
                out.push(')');
                out
            }
        }
    }
}

impl Pattern {
    /// `_`, `x`, `(Stage::Accepted tick=t)`, `(| A B)`.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        match &self.kind {
            PatternKind::Wild => "_".to_string(),
            PatternKind::Name(n) => n.clone(),
            PatternKind::Path(p) => path_text(p),
            PatternKind::Lit(PatLit::Int(i)) => i.to_string(),
            PatternKind::Lit(PatLit::Float(f)) => format!("{f:?}"),
            PatternKind::Lit(PatLit::Bool(b)) => b.to_string(),
            PatternKind::Lit(PatLit::Str(s)) => format!("{s:?}"),
            PatternKind::Ctor(path, fields) => {
                let mut out = format!("({}", path_text(path));
                for f in fields {
                    out.push(' ');
                    if let Some(name) = &f.name {
                        out.push_str(name);
                        out.push('=');
                    }
                    out.push_str(&f.pattern.dump());
                }
                out.push(')');
                out
            }
            PatternKind::Or(alts) => {
                let mut out = "(|".to_string();
                for a in alts {
                    out.push(' ');
                    out.push_str(&a.dump());
                }
                out.push(')');
                out
            }
        }
    }
}

fn dump_args(out: &mut String, args: &[Arg]) {
    for a in args {
        out.push(' ');
        if let Some(name) = &a.name {
            out.push_str(name);
            out.push('=');
        }
        a.value.dump_into(out);
    }
}

/// `(block s1 s2 …)`.
fn dump_block(out: &mut String, body: &[Stmt]) {
    out.push_str("(block");
    for s in body {
        out.push(' ');
        s.dump_into(out);
    }
    out.push(')');
}

impl Stmt {
    /// `(let x 1)`, `(= x 2)`, `(if c (block …) (else (block …)))`, … — the form the parser tests compare.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        match &self.kind {
            StmtKind::Let(l) => l.dump_into(out),
            StmtKind::Assign(a, b) => {
                out.push_str("(= ");
                a.dump_into(out);
                out.push(' ');
                b.dump_into(out);
                out.push(')');
            }
            StmtKind::Expr(e) => e.dump_into(out),
            StmtKind::If(branches, other) => {
                for (i, (cond, body)) in branches.iter().enumerate() {
                    out.push_str(if i == 0 { "(if " } else { " (elif " });
                    cond.dump_into(out);
                    out.push(' ');
                    dump_block(out, body);
                    if i > 0 {
                        out.push(')');
                    }
                }
                if let Some(body) = other {
                    out.push_str(" (else ");
                    dump_block(out, body);
                    out.push(')');
                }
                out.push(')');
            }
            StmtKind::While(cond, body) => {
                out.push_str("(while ");
                cond.dump_into(out);
                out.push(' ');
                dump_block(out, body);
                out.push(')');
            }
            StmtKind::For(vars, iter, body) => {
                let _ = write!(out, "(for ({}) ", vars.join(" "));
                iter.dump_into(out);
                out.push(' ');
                dump_block(out, body);
                out.push(')');
            }
            StmtKind::Match(e, cases) => {
                out.push_str("(match ");
                e.dump_into(out);
                for c in cases {
                    let _ = write!(out, " (case {}", c.pattern.dump());
                    if let Some(g) = &c.guard {
                        out.push_str(" (if ");
                        g.dump_into(out);
                        out.push(')');
                    }
                    out.push(' ');
                    dump_block(out, &c.body);
                    out.push(')');
                }
                out.push(')');
            }
            StmtKind::Pass => out.push_str("(pass)"),
            StmtKind::Break => out.push_str("(break)"),
            StmtKind::Continue => out.push_str("(continue)"),
            StmtKind::Return(None) => out.push_str("(return)"),
            StmtKind::Return(Some(e)) => {
                out.push_str("(return ");
                e.dump_into(out);
                out.push(')');
            }
            StmtKind::Raise(e) => {
                out.push_str("(raise ");
                e.dump_into(out);
                out.push(')');
            }
            StmtKind::Jump(name) => {
                let _ = write!(out, "(jump {name})");
            }
        }
    }
}

impl Let {
    fn dump_into(&self, out: &mut String) {
        let _ = write!(out, "(let {}", self.name);
        if let Some(ty) = &self.ty {
            let _ = write!(out, ": {}", ty.dump());
        }
        out.push(' ');
        self.value.dump_into(out);
        out.push(')');
    }
}

fn dump_fields(out: &mut String, fields: &[Field]) {
    for f in fields {
        let _ = write!(out, " ({} {})", f.name, f.ty.dump());
    }
}

fn dump_variants(out: &mut String, variants: &[Variant]) {
    for v in variants {
        if v.fields.is_empty() {
            let _ = write!(out, " {}", v.name);
        } else {
            let _ = write!(out, " ({}", v.name);
            dump_fields(out, &v.fields);
            out.push(')');
        }
    }
}

fn dump_params(out: &mut String, receiver: Option<Receiver>, params: &[Param]) {
    let mut parts: Vec<String> = Vec::new();
    match receiver {
        Some(Receiver::Value) => parts.push("self".to_string()),
        Some(Receiver::Ref) => parts.push("&self".to_string()),
        None => {}
    }
    for p in params {
        parts.push(match &p.ty {
            Some(ty) => format!("{}: {}", p.name, ty.dump()),
            None => p.name.clone(),
        });
    }
    let _ = write!(out, "({})", parts.join(", "));
}

fn dump_items(out: &mut String, items: &[Item]) {
    for i in items {
        out.push(' ');
        i.dump_into(out);
    }
}

impl File {
    /// One item per line (a test aid).
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        self.items
            .iter()
            .map(Item::dump)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Item {
    /// `(@on PlayerClicked) pub (fn greet (e) …)`: the decorators, `pub`, then the item.
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        for d in &self.decorators {
            let _ = write!(out, "(@{}", d.name);
            dump_args(out, &d.args);
            out.push_str(") ");
        }
        if self.public {
            out.push_str("pub ");
        }
        match &self.kind {
            ItemKind::Fn(f) => {
                let _ = write!(out, "(fn {} ", f.name);
                dump_params(out, f.receiver, &f.params);
                if let Some(ret) = &f.ret {
                    let _ = write!(out, " -> {}", ret.dump());
                }
                if let Some(raises) = &f.raises {
                    let _ = write!(out, " raises {}", path_text(raises));
                }
                for s in &f.body {
                    out.push(' ');
                    s.dump_into(out);
                }
                out.push(')');
            }
            ItemKind::Struct(name, fields) => {
                let _ = write!(out, "(struct {name}");
                dump_fields(out, fields);
                out.push(')');
            }
            ItemKind::Event(name, fields) => {
                let _ = write!(out, "(event {name}");
                dump_fields(out, fields);
                out.push(')');
            }
            ItemKind::Impl(name, items) => {
                let _ = write!(out, "(impl {name}");
                dump_items(out, items);
                out.push(')');
            }
            ItemKind::Enum(name, variants) => {
                let _ = write!(out, "(enum {name}");
                dump_variants(out, variants);
                out.push(')');
            }
            ItemKind::ErrorSet(name, variants) => {
                let _ = write!(out, "(error {name}");
                dump_variants(out, variants);
                out.push(')');
            }
            ItemKind::Mod(name) => {
                let _ = write!(out, "(mod {name})");
            }
            ItemKind::Use(imports) => {
                out.push_str("(use");
                for u in imports {
                    match &u.alias {
                        None => {
                            let _ = write!(out, " {}", path_text(&u.path));
                        }
                        Some(alias) => {
                            let _ = write!(out, " (as {} {alias})", path_text(&u.path));
                        }
                    }
                }
                out.push(')');
            }
            ItemKind::Let(l) => l.dump_into(out),
            ItemKind::Dialogue(d) | ItemKind::Scene(d) => d.dump_into(out),
            ItemKind::Quest(q) => {
                let _ = write!(out, "(quest {}", q.name);
                if q.repeatable {
                    out.push_str(" repeatable");
                }
                for a in &q.options {
                    out.push(' ');
                    match &a.name {
                        Some(n) => {
                            let _ = write!(out, "({n} ");
                            a.value.dump_into(out);
                            out.push(')');
                        }
                        None => a.value.dump_into(out),
                    }
                }
                if let Some(at) = &q.at {
                    let _ = write!(out, " at {at}");
                }
                dump_items(out, &q.body);
                out.push(')');
            }
            ItemKind::Role(r) => {
                let _ = write!(out, "(role {}", r.name);
                if r.optional {
                    out.push_str(" optional");
                }
                let _ = write!(out, " {}", r.ty.dump());
                if let Some(cond) = &r.cond {
                    out.push_str(" (where ");
                    cond.dump_into(out);
                    out.push(')');
                }
                out.push(')');
            }
            ItemKind::Stage(name, items) => {
                let _ = write!(out, "(stage {name}");
                dump_items(out, items);
                out.push(')');
            }
            ItemKind::Objective(o) => {
                let _ = write!(out, "(objective {}", o.name);
                if o.optional {
                    out.push_str(" optional");
                }
                out.push(' ');
                o.text.dump_into(out);
                dump_line_id(out, o.id.as_ref());
                for (word, e) in [("at", &o.at), ("count", &o.count)] {
                    if let Some(e) = e {
                        let _ = write!(out, " ({word} ");
                        e.dump_into(out);
                        out.push(')');
                    }
                }
                out.push(')');
            }
            ItemKind::Requires(cond) => {
                out.push_str("(requires ");
                cond.dump_into(out);
                out.push(')');
            }
            ItemKind::Transition(stage, cond) => {
                let _ = write!(out, "(transition {stage} ");
                cond.dump_into(out);
                out.push(')');
            }
            ItemKind::Part(p) => {
                let anchor = match p.anchor {
                    PartAnchor::On => "on",
                    PartAnchor::At => "at",
                };
                let _ = write!(out, "(part {} {anchor} {}", p.name, p.role);
                dump_items(out, &p.body);
                out.push(')');
            }
        }
    }
}

fn dump_line_id(out: &mut String, id: Option<&LineId>) {
    if let Some(id) = id {
        let _ = write!(out, " {}", id.path.join("."));
    }
}

fn dump_steps(out: &mut String, steps: &[Step]) {
    for s in steps {
        out.push(' ');
        s.dump_into(out);
    }
}

impl Cond {
    fn dump_into(&self, out: &mut String) {
        match self {
            Cond::Expr(e) => e.dump_into(out),
            Cond::Once { per, guard } => {
                out.push_str("(once");
                if let Some(per) = per {
                    out.push_str(" per ");
                    per.dump_into(out);
                }
                if let Some(guard) = guard {
                    out.push_str(" if ");
                    guard.dump_into(out);
                }
                out.push(')');
            }
        }
    }
}

impl Line {
    /// `(line npc "text" farmer.start (ticks 60) (when …))`.
    fn dump_into(&self, out: &mut String) {
        out.push_str("(line ");
        if let Some(speaker) = &self.speaker {
            out.push_str(speaker);
            out.push(' ');
        }
        self.text.dump_into(out);
        dump_line_id(out, self.id.as_ref());
        if let Some(ticks) = &self.ticks {
            out.push_str(" (ticks ");
            ticks.dump_into(out);
            out.push(')');
        }
        if let Some(cond) = &self.cond {
            out.push_str(" (when ");
            cond.dump_into(out);
            out.push(')');
        }
        out.push(')');
    }
}

impl Dialogue {
    fn dump_into(&self, out: &mut String) {
        let word = match self.kind {
            DialogueKind::Conversation => "dialogue",
            DialogueKind::Scene => "scene",
        };
        let _ = write!(out, "({word} {} ", self.name);
        dump_params(out, None, &self.params);
        for (word, e) in [
            ("for", &self.target),
            ("where", &self.filter),
            ("where", &self.cast_cond),
        ] {
            if let Some(e) = e {
                let _ = write!(out, " ({word} ");
                e.dump_into(out);
                out.push(')');
            }
        }
        if self.uninterruptible {
            out.push_str(" uninterruptible");
        }
        if let Some(topic) = &self.topic {
            out.push_str(" (topic ");
            topic.text.dump_into(out);
            dump_line_id(out, topic.id.as_ref());
            if let Some(cond) = &topic.cond {
                out.push_str(" (if ");
                cond.dump_into(out);
                out.push(')');
            }
            if let Some(priority) = topic.priority {
                let _ = write!(out, " (priority {priority})");
            }
            out.push(')');
        }
        if !self.exits.is_empty() {
            out.push_str(" (exits");
            for (exit, _) in &self.exits {
                out.push(' ');
                out.push_str(exit);
            }
            out.push(')');
        }
        for entry in &self.body {
            out.push(' ');
            match entry {
                DialogueEntry::Step(s) => s.dump_into(out),
                DialogueEntry::Node(n) => {
                    let _ = write!(out, "(node {}", n.name);
                    if let Some(cond) = &n.cond {
                        out.push_str(" (when ");
                        cond.dump_into(out);
                        out.push(')');
                    }
                    dump_steps(out, &n.steps);
                    out.push(')');
                }
            }
        }
        out.push(')');
    }
}

impl Step {
    /// `(line …)`, `(choice …)`, `(group (=> …) …)`, `(jump start)`, … (a test aid).
    #[doc(hidden)]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        self.dump_into(&mut out);
        out
    }

    fn dump_into(&self, out: &mut String) {
        match &self.kind {
            StepKind::Line(line) => line.dump_into(out),
            StepKind::Choice(args, options) => {
                out.push_str("(choice");
                dump_args(out, args);
                for o in options {
                    out.push_str(" (option");
                    match o.mark {
                        Some(OptionMark::Once) => out.push_str(" once"),
                        Some(OptionMark::Default) => out.push_str(" default"),
                        None => {}
                    }
                    if let Some(text) = &o.text {
                        out.push(' ');
                        text.dump_into(out);
                    }
                    dump_line_id(out, o.id.as_ref());
                    if let Some(guard) = &o.guard {
                        out.push_str(" (if ");
                        guard.dump_into(out);
                        out.push(')');
                    }
                    dump_steps(out, &o.steps);
                    out.push(')');
                }
                out.push(')');
            }
            StepKind::Group(members) => {
                out.push_str("(group");
                for m in members {
                    out.push_str(" (=> ");
                    m.line.dump_into(out);
                    dump_steps(out, &m.steps);
                    out.push(')');
                }
                out.push(')');
            }
            StepKind::When(cond, steps) => {
                out.push_str("(when ");
                cond.dump_into(out);
                dump_steps(out, steps);
                out.push(')');
            }
            StepKind::Once(per, steps) => {
                out.push_str("(once");
                if let Some(per) = per {
                    out.push_str(" per ");
                    per.dump_into(out);
                }
                dump_steps(out, steps);
                out.push(')');
            }
            StepKind::Jump(name) => {
                let _ = write!(out, "(jump {name})");
            }
            StepKind::Detour(name, arms) => {
                let _ = write!(out, "(detour {name}");
                for a in arms {
                    let _ = write!(out, " (arm {}", a.exit);
                    dump_steps(out, &a.steps);
                    out.push(')');
                }
                out.push(')');
            }
            StepKind::Emit(path, args) => {
                let _ = write!(out, "(emit {}", path_text(path));
                dump_args(out, args);
                out.push(')');
            }
            StepKind::Return(None) => out.push_str("(return)"),
            StepKind::Return(Some(exit)) => {
                let _ = write!(out, "(return {exit})");
            }
            StepKind::Pause(e) => {
                out.push_str("(pause ");
                e.dump_into(out);
                out.push(')');
            }
        }
    }
}
