//! Throwaway: stand-in for the Rust wasm client. The guest mod is a SEPARATE wasm instance run by
//! the browser engine; its metering imports are bound directly to the h_* exports below.
#![allow(static_mut_refs)]
static mut GAS: u64 = u64::MAX;
static mut USED: u64 = 0;
static mut STACK: u64 = 0;
const STACK_LIMIT: u64 = 64 * 1024;
static mut BUF: [u8; 65536] = [7; 65536];

#[link(wasm_import_module = "glue")]
extern "C" {
    fn mod_tick() -> u32;
    fn mod_noop(x: u32) -> u32;
    fn copy_to_guest(guest_ptr: u32, host_ptr: *const u8, len: u32);
    fn copy_from_guest(host_ptr: *mut u8, guest_ptr: u32, len: u32);
}

#[no_mangle] pub extern "C" fn h_gas(n: u64) { unsafe { if GAS < n { core::arch::wasm32::unreachable() } GAS -= n; USED += n; } }
#[no_mangle] pub extern "C" fn h_stack(a: u64, b: u64) { unsafe { STACK += a + b; if STACK > STACK_LIMIT { core::arch::wasm32::unreachable() } } }
#[no_mangle] pub extern "C" fn h_unstack(a: u64, b: u64) { unsafe { STACK -= a + b; } }
#[no_mangle] pub extern "C" fn h_memop(n: u32, _a: u64, _b: u64) -> u32 { n }
#[no_mangle] pub extern "C" fn h_host_add(a: u32, b: u32) -> u32 { a.wrapping_add(b) }
#[no_mangle] pub extern "C" fn set_gas(n: f64) { unsafe { GAS = n as u64; USED = 0; STACK = 0; } }
#[no_mangle] pub extern "C" fn set_gas_max() { unsafe { GAS = u64::MAX; USED = 0; STACK = 0; } }
#[no_mangle] pub extern "C" fn used() -> f64 { unsafe { USED as f64 } }

#[no_mangle] pub extern "C" fn run_ticks(n: u32) -> u32 { let mut s = 0; for _ in 0..n { s = unsafe { mod_tick() }; } s }
#[no_mangle] pub extern "C" fn run_noops(n: u32) -> u32 { let mut s = 0u32; for i in 0..n { s = s.wrapping_add(unsafe { mod_noop(i) }); } s }
/// n round trips of `len` bytes host -> guest -> host through the JS glue.
#[no_mangle] pub extern "C" fn run_copies(n: u32, guest_ptr: u32, len: u32) -> u32 {
    unsafe { for _ in 0..n { copy_to_guest(guest_ptr, BUF.as_ptr(), len); copy_from_guest(BUF.as_mut_ptr(), guest_ptr, len); } BUF[0] as u32 }
}
