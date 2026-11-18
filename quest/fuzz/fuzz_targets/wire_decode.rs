//! Any bytes as a wire value: `WireValue::decode` is `Ok` or `Err`, and an `Ok(v)` re-encodes to bytes that
//! decode to `v`. A float compares by `f64` `==`, so a NaN is never equal to itself: the round trip is also
//! held to the bytes — `v` re-encoded, decoded and encoded again gives the same bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use quest_vm::wire::WireValue;

/// Whether `v` holds a float NaN anywhere (iteratively: a value can nest arbitrarily deep).
fn has_nan(v: &WireValue) -> bool {
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            WireValue::Float(f) if f.is_nan() => return true,
            WireValue::List(items) => stack.extend(items),
            WireValue::Dict(pairs) => stack.extend(pairs.iter().flat_map(|(k, v)| [k, v])),
            WireValue::Struct { fields, .. } | WireValue::Variant { fields, .. } => {
                stack.extend(fields.iter().map(|(_, v)| v))
            }
            _ => {}
        }
    }
    false
}

fuzz_target!(|data: &[u8]| {
    let Ok(v) = WireValue::decode(data) else {
        return;
    };
    let mut bytes = Vec::new();
    v.encode(&mut bytes);
    let again = WireValue::decode(&bytes).expect("an encoded value decodes");
    assert!(
        has_nan(&v) || again == v,
        "the round trip changed the value"
    );
    let mut bytes_again = Vec::new();
    again.encode(&mut bytes_again);
    assert_eq!(bytes, bytes_again, "the round trip changed the bytes");
});
