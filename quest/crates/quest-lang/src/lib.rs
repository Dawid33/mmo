//! The quest script language's compiler.

pub mod ast;
mod callgraph;
pub mod diag;
mod exhaust;
pub mod hir;
mod indent;
pub mod infer;
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

use quest_vm::host::HostApi;
use std::collections::BTreeMap;

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

/// Checks a program against the host's API with every compiler stage built so far (plan D10): `Ok` with
/// the warnings, or `Err` with every diagnostic when any is an error; either way sorted by file, line and
/// column.
///
/// Dialogues and quests (decision 19) are resolved and checked, then each gets one `not-yet` and is
/// dropped before the later stages: the call graph and type inference ([`infer`]).
pub fn check(tree: &SourceTree, host: &HostApi) -> Result<Vec<Diagnostic>, Vec<Diagnostic>> {
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
    let (_typed, mut inferred) = infer(&program, host);
    diags.append(&mut inferred);
    diags.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
    if diags.iter().any(|d| d.severity == Severity::Error) {
        Err(diags)
    } else {
        Ok(diags)
    }
}
