//! Allocation regression for the lexer. Its own test binary with a counting global allocator, and a single
//! test, so nothing else allocates while the measured window is open.
#![cfg(not(target_arch = "wasm32"))]

use quest_lang::lex;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Allocated bytes per input byte that the lexer may use. The token list, positions and the layout pass
/// need about 50 bytes per input byte (a few dozen per token) (one token per 2 input bytes here); copying the rest of the input per
/// number token allocated ~input_len^2 / 8 bytes, tens of thousands of times more.
const MAX_ALLOCATED_PER_INPUT_BYTE: usize = 128;

/// Regression: number lexing copied the whole remaining input per number token (a 1 MiB line took ~3.5 s).
/// Counts allocated bytes, not time, which a loaded machine inflates.
#[test]
fn a_megabyte_line_lexes() {
    let text = "1 + ".repeat(1 << 18);
    let region = Region::new(GLOBAL);
    let (tokens, diags) = lex("m", &text);
    let allocated = region.change().bytes_allocated;
    assert!(diags.is_empty());
    assert_eq!(tokens.len(), (1 << 19) + 2);
    assert!(
        allocated <= MAX_ALLOCATED_PER_INPUT_BYTE * text.len(),
        "{allocated} bytes allocated lexing {} bytes",
        text.len()
    );
    println!("allocated {allocated} for {} input bytes", text.len());
}
