//! Generates the parser from `src/grammar.lalrpop` into `OUT_DIR`. LALRPOP fails the build on any
//! grammar conflict.

fn main() {
    if let Err(e) = lalrpop::process_src() {
        panic!("lalrpop: {e}");
    }
}
