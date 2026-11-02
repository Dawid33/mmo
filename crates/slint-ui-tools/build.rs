//! Compiles the asset viewer and the controls test harness (`ui/index.slint`,
//! which imports them from crates/slint-ui/ui).
include!("../slint-ui/build_common.rs");

fn main() {
    compile_ui("ui/index.slint");
}
