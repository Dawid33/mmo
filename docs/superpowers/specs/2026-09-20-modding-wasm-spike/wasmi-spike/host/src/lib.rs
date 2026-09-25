use wasmi::*;

#[path = "../../guest/src/work.rs"]
pub mod work;

static GUEST: &[u8] = include_bytes!("../../guest.wasm");

pub struct Bench {
    pub store: Store<()>,
    pub init: TypedFunc<(), ()>,
    pub tick: TypedFunc<(), u32>,
    pub noop: TypedFunc<u32, u32>,
    pub hostcalls: TypedFunc<u32, u32>,
    pub memory: Memory,
    pub fuel: bool,
    pub snap: Vec<u8>,
}

impl Bench {
    pub fn new(fuel: bool) -> Self {
        let mut config = Config::default();
        config.consume_fuel(fuel);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, GUEST).unwrap();
        let mut store = Store::new(&engine, ());
        if fuel { store.set_fuel(u64::MAX).unwrap(); }
        let mut linker = <Linker<()>>::new(&engine);
        linker.func_wrap("env", "host_add", |a: u32, b: u32| -> u32 { a.wrapping_add(b) }).unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let init = instance.get_typed_func::<(), ()>(&store, "init").unwrap();
        let tick = instance.get_typed_func::<(), u32>(&store, "tick").unwrap();
        let noop = instance.get_typed_func::<u32, u32>(&store, "noop").unwrap();
        let hostcalls = instance.get_typed_func::<u32, u32>(&store, "hostcalls").unwrap();
        let memory = instance.get_memory(&store, "memory").unwrap();
        init.call(&mut store, ()).unwrap();
        Bench { store, init, tick, noop, hostcalls, memory, fuel, snap: Vec::new() }
    }
    pub fn ticks(&mut self, n: u32) -> u32 {
        let mut s = 0;
        for _ in 0..n { s = self.tick.call(&mut self.store, ()).unwrap(); }
        s
    }
    pub fn noops(&mut self, n: u32) -> u32 {
        let mut s = 0u32;
        for i in 0..n { s = s.wrapping_add(self.noop.call(&mut self.store, i).unwrap()); }
        s
    }
    pub fn hostcalls(&mut self, n: u32) -> u32 { self.hostcalls.call(&mut self.store, n).unwrap() }
    /// Snapshot + restore the whole linear memory n times (WASM-4-style rollback cost).
    pub fn snapshots(&mut self, n: u32) -> u32 {
        for _ in 0..n {
            let data = self.memory.data(&self.store);
            self.snap.clear();
            self.snap.extend_from_slice(data);
            let snap = core::mem::take(&mut self.snap);
            self.memory.data_mut(&mut self.store).copy_from_slice(&snap);
            self.snap = snap;
        }
        self.snap.len() as u32
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    static mut B: Option<Bench> = None;
    static mut ENTS: [work::Ent; work::N] = [work::ZERO; work::N];
    #[allow(static_mut_refs)]
    fn b() -> &'static mut Bench { unsafe { B.as_mut().unwrap() } }
    #[no_mangle] pub extern "C" fn w_init(fuel: u32) { unsafe { B = Some(Bench::new(fuel != 0)); } }
    #[no_mangle] pub extern "C" fn w_ticks(n: u32) -> u32 { b().ticks(n) }
    #[no_mangle] pub extern "C" fn w_noops(n: u32) -> u32 { b().noops(n) }
    #[no_mangle] pub extern "C" fn w_hostcalls(n: u32) -> u32 { b().hostcalls(n) }
    #[no_mangle] pub extern "C" fn w_snapshots(n: u32) -> u32 { b().snapshots(n) }
    // Same workload compiled straight into this module: the "host-native Rust on V8" baseline.
    #[allow(static_mut_refs)]
    #[no_mangle] pub extern "C" fn w_direct(n: u32) -> u32 {
        unsafe { work::init(&mut ENTS); let mut s = 0; for _ in 0..n { s = work::tick(&mut ENTS); } s }
    }
}
