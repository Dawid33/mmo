use water_proto::edit::{Edit, apply};
use water_proto::fixture::{fill_solid, finish};
use water_proto::grid::Dims;
use water_proto::state::World;
use water_proto::{FILM, FULL, Mode, ca};

#[test]
fn checkerboard_spill_is_unbiased() {
    let mut w = World::new(Dims::cube(64), Mode::Bodies);
    fill_solid(&mut w, 0..=3);
    finish(&mut w);
    let at = w.dims.idx(32, 8, 32);
    let mut done = false;
    for s in 0..5000u32 {
        if s <= 3 {
            apply(&mut w, Edit::Pour(at, FULL)).unwrap();
        }
        ca::step(&mut w);
        if s > 3 && w.active.count() == 0 {
            done = true;
            break;
        }
    }
    assert!(done, "not asleep");
    assert_eq!(w.mass(), 4 * FULL as u64);
    let (mut l, mut r, mut b, mut f) = (0u64, 0u64, 0u64, 0u64);
    for i in 0..w.dims.len() as u32 {
        let q = w.water[i as usize] as u64;
        if q == 0 {
            continue;
        }
        let (x, _, z) = w.dims.xyz(i);
        if x < 32 { l += q } else if x > 32 { r += q }
        if z < 32 { b += q } else if z > 32 { f += q }
    }
    assert!(l.abs_diff(r) <= FILM as u64, "x imbalance {l} vs {r}");
    assert!(b.abs_diff(f) <= FILM as u64, "z imbalance {b} vs {f}");
}
