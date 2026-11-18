//! Any bytes, read as lossy UTF-8, as a people file and as a script file of `quest run`:
//! `quest_stub::parse_people` and `parse_script` return `Ok` or `Err` without a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = quest_stub::parse_people(&text);
    let _ = quest_stub::parse_script(&text);
});
