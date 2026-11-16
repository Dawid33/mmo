//! Diagnostics and their rendering.

use crate::source::SourceTree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn word(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One problem found in a source file. `line` and `col` are 1-based; `col` and `len` count characters
/// (Unicode scalar values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub len: u32,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(
        code: &'static str,
        message: impl Into<String>,
        file: &str,
        line: u32,
        col: u32,
        len: u32,
    ) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            file: file.to_string(),
            line,
            col,
            len,
            notes: Vec::new(),
        }
    }
}

/// Renders a diagnostic as a block (no trailing newline):
///
/// ```text
/// error[recursion]: recursive call cycle: f -> f
///  --> main.qs:2:12
///   |
/// 2 |     return f(n)
///   |            ^^^^
/// ```
///
/// The gutter is as wide as the line number. A diagnostic whose line is not in the file renders
/// without the source line and the gutter bars; notes follow as `  = note: …`.
pub fn render(d: &Diagnostic, tree: &SourceTree) -> String {
    let number = d.line.to_string();
    let pad = " ".repeat(number.len());
    let mut out = format!(
        "{}[{}]: {}\n{}--> {}:{}:{}",
        d.severity.word(),
        d.code,
        d.message,
        pad,
        d.file,
        d.line,
        d.col
    );
    let source_line = tree.files.get(&d.file).and_then(|text| {
        let index = usize::try_from(d.line).ok()?.checked_sub(1)?;
        let line = text.split('\n').nth(index)?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        Some(if index == 0 {
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        })
    });
    if let Some(line) = source_line {
        let carets = "^".repeat(d.len.max(1) as usize);
        let lead = " ".repeat(d.col.saturating_sub(1) as usize);
        out.push_str(&format!(
            "\n{pad} |\n{number} | {line}\n{pad} | {lead}{carets}"
        ));
    }
    for note in &d.notes {
        out.push_str(&format!("\n{pad} = note: {note}"));
    }
    out
}
