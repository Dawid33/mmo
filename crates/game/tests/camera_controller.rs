use game::na::Vector3;
use game::parry::math::Real;
use game::{GameEventKind, InputEvent, Key, Region, RegionCoords, Rollback};
use ordered_float::OrderedFloat;
use std::hash::Hash;

fn crc(region: &Region) -> u32 {
    let mut h = crc32fast::Hasher::new();
    region.data().data.hash(&mut h);
    h.finalize()
}

/// One region, one player, FPS-cam enabled, positioned by create_player_safe.
///
/// Built via `Region::new` with a live throwaway render-event sender rather
/// than `Region::from_chunks` (which wires a `None` sender): rollback of a
/// tier-2 undo entry (our camera `register()`, and the surgical
/// `mutate_body_safe` body-write sites) unwraps that sender internally, and a
/// `None` client panics there — `Region::from_chunks` is for server regions,
/// which only ever `forget()` transactions, never `rollback()` them. Same
/// workaround as `handoff_scan.rs`'s `extracting_tick_holds_the_hash_bar`.
/// `_send` stays bound so the channel doesn't disconnect mid-test.
fn region_fps_ready() -> (Region, game::EntityKey, crossbeam::channel::Receiver<game::GameDataUpdate>)
{
    let (send, recv) = crossbeam::channel::unbounded();
    let mut region = Region::new(Rollback::new(None), Some(send), RegionCoords::new(0, 0), None);
    region.handle_event(GameEventKind::CreateClient(7)).unwrap();
    // Enable FPS cam: KeyE press, then a Tick toggles it on (controllers run
    // before the toggle on this tick, so no look happens yet).
    region
        .handle_event(GameEventKind::PlayerInput(
            7,
            InputEvent::Key { key: Key::KeyE, pressed: true },
        ))
        .unwrap();
    region.handle_event(GameEventKind::Tick).unwrap();
    let e = *region.data().player_entites.get(&7).unwrap();
    (region, e, recv)
}

#[test]
fn mouse_tick_accumulates_look_and_rebuilds_body_rotation() {
    let (mut region, e, _recv) = region_fps_ready();
    region
        .handle_event(GameEventKind::PlayerInput(
            7,
            InputEvent::MouseMotion { dx: 40.0, dy: -25.0 },
        ))
        .unwrap();
    region.handle_event(GameEventKind::Tick).unwrap();

    let cam = region.data().ecs.camera.get(e).clone();
    assert_ne!(cam.yaw, OrderedFloat(0.0), "yaw should accumulate");
    assert_ne!(cam.pitch, OrderedFloat(0.0), "pitch should accumulate");

    // Body's post-step rotation equals the derived full-look quaternion.
    let handle = *region.data().ecs.rigidbody.get(e);
    let body_rot = region.data().physics.bodies.get(handle).unwrap().rotation().clone();
    let expected = game::look_quat(cam.yaw, cam.pitch);
    assert!(
        body_rot.angle_to(&expected).into_inner() < 1e-5,
        "body rotation must match look_quat(yaw,pitch)"
    );
    // No roll: camera-right stays horizontal.
    let right = body_rot.transform_vector(&Vector3::<Real>::x());
    assert!(right.y.into_inner().abs() < 1e-5, "body look must not roll");
}

/// Movement path: KeyW-held ticks write `set_next_kinematic_translation`
/// through the surgical body-write entry. Runs under the default Full mode,
/// so each rollback whole-field-hashes the RigidBodySet — proving the
/// footprint inverse (body slot + modified-mark) misses nothing.
#[test]
fn movement_ticks_roll_back_bit_exact() {
    let (mut region, _e, _recv) = region_fps_ready();
    let mut hashes = vec![crc(&region)];
    let events = [
        GameEventKind::PlayerInput(7, InputEvent::Key { key: Key::KeyW, pressed: true }),
        GameEventKind::Tick,
        GameEventKind::Tick,
        GameEventKind::PlayerInput(7, InputEvent::Key { key: Key::KeyW, pressed: false }),
        GameEventKind::Tick,
    ];
    for ev in events {
        region.handle_event(ev).unwrap();
        hashes.push(crc(&region));
    }
    for expected in hashes.iter().rev().skip(1) {
        region.rollback_last_event();
        assert_eq!(*expected, crc(&region), "rollback hash mismatch");
    }
}

#[test]
fn look_ticks_roll_back_bit_exact() {
    let (mut region, _e, _recv) = region_fps_ready();
    // Baseline hash, then a sequence of one-event-per-transaction steps.
    let mut hashes = vec![crc(&region)];
    let events = [
        GameEventKind::PlayerInput(7, InputEvent::MouseMotion { dx: 40.0, dy: -25.0 }),
        GameEventKind::Tick,
        GameEventKind::PlayerInput(7, InputEvent::MouseMotion { dx: -15.0, dy: 30.0 }),
        GameEventKind::Tick,
        GameEventKind::PlayerInput(7, InputEvent::MouseMotion { dx: 5.0, dy: -5.0 }),
        GameEventKind::Tick,
    ];
    for ev in events {
        region.handle_event(ev).unwrap();
        hashes.push(crc(&region));
    }
    // Roll back each transaction; every boundary hash must be restored.
    for expected in hashes.iter().rev().skip(1) {
        region.rollback_last_event();
        assert_eq!(*expected, crc(&region), "rollback hash mismatch");
    }
}
