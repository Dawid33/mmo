//! The quest script language's compiler.

pub mod ast;
pub mod diag;
mod indent;
pub mod lex;
pub mod parse;
pub mod source;

pub use diag::{Diagnostic, Severity, render};
pub use lex::{FPart, Tok, Token, lex};
pub use parse::parse_file;
#[doc(hidden)]
pub use parse::{parse_expr_str, parse_pattern_str, parse_type_str};
pub use source::SourceTree;
