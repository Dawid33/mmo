//! Any bytes, read as lossy UTF-8, as the one file `main.qs` of a tree: `quest_lang::compile` returns (a
//! program or diagnostics) without a panic or a stack overflow, within the recipe's timeout.
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use quest_lang::SourceTree;
use quest_vm::HostApi;

fuzz_target!(|data: &[u8]| {
    static API: OnceLock<HostApi> = OnceLock::new();
    let api = API.get_or_init(quest_stub::stub_api);
    let mut tree = SourceTree::default();
    tree.files.insert(
        "main.qs".to_string(),
        String::from_utf8_lossy(data).into_owned(),
    );
    let _ = quest_lang::compile(&tree, api);
});
