//! Compiles the in-game screens (crates/slint-ui/ui/index.slint: GameUi,
//! HudUi). The frontend is slint-ui-frontend, the asset viewer and
//! test harness slint-ui-tools — see crates/slint-ui/build_common.rs.
include!("../slint-ui/build_common.rs");

fn main() {
    compile_ui("../slint-ui/ui/index.slint");
}
