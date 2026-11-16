//! Generates the parser from `src/grammar.lalrpop` into `OUT_DIR`. LALRPOP fails the build on any
//! grammar conflict.

fn main() {
    // Only the grammar feeds the generated parser; without this, cargo reruns the script on any change
    // in the package.
    println!("cargo:rerun-if-changed=src/grammar.lalrpop");
    if let Err(e) = lalrpop::process_src() {
        panic!("lalrpop: {e}");
    }
}
