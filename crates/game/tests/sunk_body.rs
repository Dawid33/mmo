//! A body that starts a tick sunk into terrain is put back on its surface.
//! The character controller cannot slide or step out of an overlap by
//! itself; it happens at region seams, where the region a body leaves has
//! no floor past its edge and the body's rounded bottom dips over that edge
//! on the crossing tick (about 3 units over a corner).
use crate::common::{self, FLOOR_PLATES};
use game::movement::{BodyScale, CHARACTER_SKIN};
use game::{GameEventKind, InputEvent, Key, Pose, Region, RegionCoords};

fn ev(r: &mut Region, e: GameEventKind) {
    r.handle_event(e).unwrap();
    r.forget_last_event();
}

fn body(r: &Region) -> (f32, f32, f32) {
    let d = r.data();
    let e = *d.player_entites.get(&0).unwrap();
    let t = d.physics.bodies.get(*d.ecs.rigidbody.get(e)).unwrap().translation();
    (t.x, t.y, t.z)
}

/// Plates the ground lies lower outside the floor: three tiles, nothing a body on
/// the floor's edge can touch.
const DROP_PLATES: i32 = 18;

/// Flat stud floor (top at `FLOOR_PLATES`) over x, z in 96..192; the player
/// placed at (x, y, z) walking W+A (towards -x, -z) for four ticks. Returns
/// its trail.
fn walk_from(x: f32, y: f32, z: f32) -> Vec<(f32, f32, f32)> {
    // The floor is tile columns 6..12 each way — stud columns 12..24 —, its
    // edges and corner where they are at any chunk size.
    let (n, studs) = (game::STUD_CHUNK_STUDS, 12..24usize);
    let seed = common::stud_seed_over(96.0..192.0, 96.0..192.0, |cx, cz| {
        common::stud_ground(|sx, sz| {
            let on = studs.contains(&(cx * n + sx)) && studs.contains(&(cz * n + sz));
            if on { FLOOR_PLATES } else { FLOOR_PLATES - DROP_PLATES }
        })
    });
    let mut r = common::region_of(RegionCoords::new(0, 0), seed);
    ev(&mut r, GameEventKind::CreateClient(0));
    for pressed in [true, false] {
        ev(&mut r, GameEventKind::PlayerInput(0, InputEvent::Key { key: Key::KeyE, pressed }));
    }
    for _ in 0..12 {
        ev(&mut r, GameEventKind::Tick);
    }
    r.with_data(|d| {
        let e = *d.data.player_entites.get(&0).unwrap();
        d.set_body_pose_safe(e, Pose::translation(x, y, z));
    });
    for key in [Key::KeyW, Key::KeyA] {
        ev(&mut r, GameEventKind::PlayerInput(0, InputEvent::Key { key, pressed: true }));
    }
    (0..4)
        .map(|_| {
            ev(&mut r, GameEventKind::Tick);
            body(&r)
        })
        .collect()
}

#[test]
fn a_body_sunk_into_the_floor_is_put_back_on_it_and_walks_on() {
    // On the studded floor: the stud caps, the capsule, the controller's skin.
    let s = BodyScale::STUD;
    let rest = common::floor_y(FLOOR_PLATES) + game::stud_cap_units() + s.capsule_half_height() + s.capsule_radius() + CHARACTER_SKIN;
    // Mid-floor and 4 units inside a corner; resting, over an edge, over a corner.
    for (x, z) in [(150.0, 150.0), (188.0, 188.0)] {
        for sink in [0.0, 0.86, 3.03] {
            let trail = walk_from(x, rest - sink, z);
            let at = format!("start ({x}, {}, {z}): {trail:?}", rest - sink);
            let (lx, ly, lz) = *trail.last().unwrap();
            assert!((ly - rest).abs() < 0.01, "must stand on the floor: {at}");
            assert!(lx < x - 5.0 && lz < z - 5.0, "must walk on: {at}");
        }
    }
}
