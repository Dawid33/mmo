//! The quest script language's compiler.

pub mod ast;
pub mod diag;
pub mod hir;
mod indent;
pub mod lex;
mod modules;
pub mod parse;
mod resolve;
pub mod source;

pub use diag::{Diagnostic, Severity, render};
pub use lex::{FPart, Tok, Token, lex};
pub use modules::load_modules;
pub use parse::parse_file;
#[doc(hidden)]
pub use parse::{parse_expr_str, parse_pattern_str, parse_type_str};
pub use resolve::resolve;
pub use source::SourceTree;

use quest_vm::host::HostApi;

/// Checks a program against the host's API with every compiler stage built so far (plan D10): `Ok` with
/// the warnings, or `Err` with every diagnostic when any is an error; either way sorted by file, line and
/// column.
///
/// Dialogues and quests (decision 19) are resolved and checked, then each gets one `not-yet` and is
/// dropped before the later stages.
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
    diags.sort_by(|a, b| (&a.file, a.line, a.col).cmp(&(&b.file, b.line, b.col)));
    if diags.iter().any(|d| d.severity == Severity::Error) {
        Err(diags)
    } else {
        Ok(diags)
    }
}
