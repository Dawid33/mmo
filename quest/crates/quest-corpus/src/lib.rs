//! The quest script program corpus (plan Task 32; spec §11 "Program corpus", §6.7): every example of the
//! spec as a program under `quest/corpus/run/<case>/` with its hand-written output (`expect.txt`) and its
//! golden trace (`golden.txt`: bytecode hash, per-tick fuel and state hash, final memory hash, output,
//! reports), and the diagnostic cases under `quest/corpus/fail/<case>/` with their rendered diagnostics.
//!
//! The build script embeds the corpus, so the crate needs no filesystem at run time and the same cases run
//! on wasm32 ([`RUN`], [`FAIL`]). A run case may start `main.qs` with a config line, see [`config_of`].
#![forbid(unsafe_code)]

mod harness;

pub use harness::{CaseConfig, config_of, diagnostics_of, expect_text_of, golden_of, tree_of};

/// One corpus case, embedded.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    /// The case's directory name.
    pub name: &'static str,
    /// Every `*.qs` file of the case: path relative to the case (`/` separators), text. Sorted by path.
    pub files: &'static [(&'static str, &'static str)],
    /// `expect.txt`.
    pub expect: &'static str,
    /// `golden.txt` (run cases; `None` until blessed).
    pub golden: Option<&'static str>,
}

include!(concat!(env!("OUT_DIR"), "/corpus.rs"));
