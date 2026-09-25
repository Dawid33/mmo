use host::{work, Bench};
use std::time::Instant;

fn time<T: std::fmt::Debug>(label: &str, per: u32, f: impl FnOnce() -> T) {
    let t = Instant::now();
    let r = f();
    let us = t.elapsed().as_secs_f64() * 1e6;
    println!("{label:<40} total {:>10.1} ms   per-unit {:>10.3} us   result {:?}", us / 1e3, us / per as f64, r);
}

fn main() {
    const TICKS: u32 = 2000;
    let mut ents = Box::new([work::ZERO; work::N]);
    work::init(&mut ents);
    time("native rust direct: tick(1024 ents)", TICKS, || { let mut s = 0; for _ in 0..TICKS { s = work::tick(&mut ents); } s });
    for fuel in [false, true] {
        let mut b = Bench::new(fuel);
        let tag = if fuel { "wasmi native +fuel" } else { "wasmi native" };
        time(&format!("{tag}: tick(1024 ents)"), TICKS, || b.ticks(TICKS));
        time(&format!("{tag}: noop call"), 1_000_000, || b.noops(1_000_000));
        time(&format!("{tag}: host import call"), 1_000_000, || b.hostcalls(1_000_000));
        time(&format!("{tag}: mem snapshot+restore"), 10_000, || b.snapshots(10_000));
    }
}
