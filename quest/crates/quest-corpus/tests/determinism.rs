//! Cross-platform determinism (spec §6.7, §11; plan Task 34): what the corpus recorded on native — the
//! goldens (bytecode hash, per-tick fuel and state hash, final memory hash, output, reports) and the
//! rendered diagnostics — is reproduced exactly on wasm32 (`just quest-wasm` runs this file on both). The
//! allocator's addresses and float results are pinned by literals taken from the first native run.
//!
//! A difference on wasm32 is fixed at its source (a `usize` reaching a hash or bytes, a platform float
//! function, an unordered container), never by recording a golden per target.

use quest_corpus::{Case, CaseConfig, FAIL, RUN, diagnostics_of, golden_of, trace_of};
use quest_vm::Store;
use quest_vm::alloc::{Heap, Kind};

/// Every case whose text differs from what it should be, shown with both texts.
fn mismatches(
    cases: &[Case],
    actual: impl Fn(&Case) -> String,
    expected: impl Fn(&Case) -> String,
) {
    let mut bad = Vec::new();
    for case in cases {
        let (got, want) = (actual(case), expected(case));
        if got != want {
            bad.push(format!(
                "=== {}\n--- expected\n{want}--- got\n{got}",
                case.name
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} case(s) differ:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn goldens_reproduce() {
    mismatches(RUN, golden_of, |c| {
        c.golden.unwrap_or("(no golden)\n").to_string()
    });
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn fail_diagnostics_reproduce() {
    mismatches(FAIL, diagnostics_of, |c| c.expect.to_string());
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn allocator_addresses_reproduce() {
    let mut st = Store::new(16, 65_536).expect("a store");
    let h = Heap {
        base: st.layout.heap_base,
        end: st.layout.heap_end,
        script: true,
    };
    h.init(&mut st).expect("init");
    let alloc = |st: &mut Store, kind: Kind, words: u32| {
        h.alloc(st, kind, words)
            .expect("no memory error")
            .expect("room")
    };
    // Mixed classes, frees in between (LIFO reuse per class), then growth past the freed blocks.
    let mut got = Vec::new();
    let a = alloc(&mut st, Kind::Str, 1);
    let b = alloc(&mut st, Kind::ListImm, 5);
    let c = alloc(&mut st, Kind::Raw, 0);
    let d = alloc(&mut st, Kind::Dict, 30);
    got.extend([a, b, c, d]);
    h.free(&mut st, b).expect("free b");
    h.free(&mut st, a).expect("free a");
    let e = alloc(&mut st, Kind::Obj, 2);
    let f = alloc(&mut st, Kind::Str, 6);
    let g = alloc(&mut st, Kind::Raw, 100);
    got.extend([e, f, g]);
    h.free(&mut st, d).expect("free d");
    h.free(&mut st, e).expect("free e");
    let i = alloc(&mut st, Kind::ListRef, 29);
    let j = alloc(&mut st, Kind::Str, 0);
    let k = alloc(&mut st, Kind::Raw, 1);
    got.extend([i, j, k]);
    assert_eq!(got, ALLOC_ADDRESSES, "allocator addresses");
}

/// [`allocator_addresses_reproduce`]'s addresses, from the first native run.
const ALLOC_ADDRESSES: [u32; 10] = [920, 952, 1016, 1032, 920, 952, 1288, 1032, 2312, 920];

/// A program printing the float results pinned below, one per line.
const FLOATS: Case = Case {
    name: "floats",
    files: &[(
        "main.qs",
        "fn main():\n    print(f\"{sqrt(2.0)}\")\n    print(f\"{sin(1.0)}\")\n    print(f\"{cos(1.0)}\")\n    print(f\"{pow(2.0, 0.5)}\")\n    print(f\"{0.1 + 0.2}\")\n",
    )],
    expect: "",
    golden: None,
    people: None,
    script: None,
};

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn float_bits_reproduce() {
    let trace = trace_of(&FLOATS, &CaseConfig::default()).expect("compiles and loads");
    // A float prints shortest round-trip, so parsing the text gives back its exact bits.
    let bits: Vec<u64> = trace
        .output
        .iter()
        .map(|line| {
            line.parse::<f64>()
                .unwrap_or_else(|e| panic!("{line:?}: {e}"))
                .to_bits()
        })
        .collect();
    assert_eq!(
        bits, FLOAT_BITS,
        "sqrt(2), sin(1), cos(1), pow(2, 0.5), 0.1 + 0.2 (output {:?})",
        trace.output
    );
}

/// [`float_bits_reproduce`]'s bit patterns, from the first native run.
const FLOAT_BITS: [u64; 5] = [
    0x3ff6_a09e_667f_3bcd,
    0x3fea_ed54_8f09_0cee,
    0x3fe1_4a28_0fb5_068c,
    0x3ff6_a09e_667f_3bcd,
    0x3fd3_3333_3333_3334,
];
