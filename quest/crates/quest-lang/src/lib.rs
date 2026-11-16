//! The quest script language's compiler.

pub mod ast;
mod callgraph;
mod codegen;
pub mod diag;
mod exhaust;
pub mod hir;
mod indent;
pub mod infer;
mod layout;
pub mod lex;
mod modules;
pub mod parse;
mod resolve;
pub mod source;
pub mod ty;

pub use diag::{Diagnostic, Severity, render};
pub use infer::{
    Await, BuiltinMethod, Method, PatCtor, PatInfo, Place, PlaceRoot, PlaceStep, Sig, Typed, infer,
};
pub use lex::{FPart, Tok, Token, lex};
pub use modules::load_modules;
pub use parse::parse_file;
#[doc(hidden)]
pub use parse::{parse_expr_str, parse_pattern_str, parse_type_str};
pub use resolve::resolve;
pub use source::SourceTree;

use quest_vm::Program;
use quest_vm::host::HostApi;
use std::collections::BTreeMap;

/// Test aid: deterministic work counters for the regression tests that guard against quadratic behaviour
/// (a wall-clock bound fails on a loaded machine). Per thread; a counter is one increment per unit of work.
#[doc(hidden)]
pub mod work {
    use std::cell::Cell;

    thread_local! {
        static TYPE_VISITS: Cell<u64> = const { Cell::new(0) };
    }

    /// Zeroes this thread's counters.
    pub fn reset() {
        TYPE_VISITS.with(|c| c.set(0));
    }

    /// Type nodes visited by type inference's walks (`shallow`, occurs check, `zonk`) since `reset`.
    pub fn type_visits() -> u64 {
        TYPE_VISITS.with(Cell::get)
    }

    pub(crate) fn add_type_visit() {
        TYPE_VISITS.with(|c| c.set(c.get() + 1));
    }
}

/// Test aid: every function's signature by qualified name (`fn(int) -> int`), whatever the diagnostics.
#[doc(hidden)]
pub fn signatures(tree: &SourceTree, host: &HostApi) -> BTreeMap<String, String> {
    let (krate, _) = load_modules(tree);
    let (program, _) = resolve(&krate, host);
    let (typed, _) = infer(&program, host);
    typed
        .sigs
        .iter()
        .map(|(d, sig)| {
            (
                program.qualified_name(*d),
                typed.print_sig(sig, &program, host),
            )
        })
        .collect()
}

/// A compiled program and the warnings its checks gave.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub program: Program,
    pub warnings: Vec<Diagnostic>,
}

/// The front end: modules, resolution and type inference; every diagnostic, sorted by file, line and
/// column. Dialogues and quests (decision 19) are resolved and checked, then each gets one `not-yet` and is
/// dropped before the later stages: the call graph and type inference ([`infer`]).
fn front(tree: &SourceTree, host: &HostApi) -> (hir::Crate, Typed, Vec<Diagnostic>) {
    let (krate, mut diags) = load_modules(tree);
    let (program, mut resolved) = resolve(&krate, host);
    diags.append(&mut resolved);
    for id in &program.deferred {
        let Some(def) = program.def(*id) else {
            continue;
        };
        let file = program
            .module(def.module)
            .map_or(String::new(), |m| m.file.clone());
        let noun = match def.kind {
            hir::DefKind::Quest => "quest",
            _ => "dialogue",
        };
        diags.push(Diagnostic::error(
            "not-yet",
            format!(
                "{noun} `{}` is checked but not compiled yet (dialogue and quests come in a later version)",
                def.name
            ),
            &file,
            def.span.line,
            def.span.col,
            def.span.len,
        ));
    }
    let (typed, mut inferred) = infer(&program, host);
    diags.append(&mut inferred);
    sort_diags(&mut diags);
    (program, typed, diags)
}

fn sort_diags(diags: &mut [Diagnostic]) {
    diags.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
}

fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

/// Checks a program against the host's API with every compiler stage before layout and code generation
/// (plan D10): `Ok` with the warnings, or `Err` with every diagnostic when any is an error; either way
/// sorted by file, line and column. What only code generation finds (`too-many-slots`, and `not-yet` for a
/// construct the code generator does not compile yet) is reported by [`compile`].
pub fn check(tree: &SourceTree, host: &HostApi) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
    let (_, _, diags) = front(tree, host);
    if has_errors(&diags) {
        Err(diags)
    } else {
        Ok(diags)
    }
}

/// Compiles a program against the host's API (plan D10): `check`, then layout and code generation. `Err`
/// with every diagnostic (sorted as `check`'s) when any is an error; code generation runs only on a program
/// the checks accepted.
pub fn compile(tree: &SourceTree, host: &HostApi) -> Result<Compiled, Vec<Diagnostic>> {
    let (krate, typed, diags) = front(tree, host);
    if has_errors(&diags) {
        return Err(diags);
    }
    match codegen::generate(&krate, &typed, host) {
        Ok(program) => Ok(Compiled {
            program,
            warnings: diags,
        }),
        Err(mut errors) => {
            errors.extend(diags);
            sort_diags(&mut errors);
            Err(errors)
        }
    }
}
