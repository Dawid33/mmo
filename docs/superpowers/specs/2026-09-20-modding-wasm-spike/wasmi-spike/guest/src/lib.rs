#![no_std]
mod work;
use work::{Ent, N, ZERO};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }

#[link(wasm_import_module = "env")]
extern "C" { fn host_add(a: u32, b: u32) -> u32; }

static mut ENTS: [Ent; N] = [ZERO; N];

#[no_mangle] pub extern "C" fn init() { unsafe { work::init(&mut *core::ptr::addr_of_mut!(ENTS)) } }
#[no_mangle] pub extern "C" fn tick() -> u32 { unsafe { work::tick(&mut *core::ptr::addr_of_mut!(ENTS)) } }
#[no_mangle] pub extern "C" fn noop(x: u32) -> u32 { x }
#[no_mangle] pub extern "C" fn hostcalls(n: u32) -> u32 {
    let mut acc = 0u32;
    for i in 0..n { acc = unsafe { host_add(acc, i) }; }
    acc
}
/// Bits of a computed float: 0.0/0.0 yields an engine/CPU-defined NaN unless canonicalized.
#[no_mangle] pub extern "C" fn divbits(a: f32, b: f32) -> u32 { (a / b).to_bits() }
