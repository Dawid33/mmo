//! Compiles the frontend screens (`frontend.slint` in crates/slint-ui/ui).
include!("../slint-ui/build_common.rs");

fn main() {
    compile_ui("../slint-ui/ui/frontend.slint");
}
