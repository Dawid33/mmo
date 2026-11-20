//! Headless Bevy-App harness driving the REAL client input pipeline:
//! `ButtonInput<KeyCode>` -> `forward_input` -> game channel ->
//! `GameInstanceManager` gates -> sim tick -> `SetFreeCam` ->
//! `drain_region_updates` -> `CursorGrabMode`. The Bevy-layer complement to
//! `SimHarness`: no window, no GPU, no threads, no wall clock. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use std::time::Duration;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow, Window};
use crossbeam::channel::{Receiver, unbounded};
use game::{ClientId, ClientUpdateEvent, GameEventKind, InputEvent, Key, ServerPacket};

use crate::renderer::bridge::{
    RegionRoots, Regions, RetiredRegionRoots, SimEntityMap, SnapshotVoxelQueue,
    apply_snapshot_voxels, despawn_retired_regions, drain_client_updates, drain_region_updates,
};
use crate::renderer::hud::HudStatus;
use crate::renderer::input::forward_input;
use crate::renderer::slint_overlay::UiUpdates;
use crate::renderer::{ClientUpdates, GameEvents, LocalPlayer};
use crate::{GameInstanceManager, LocalServer};

pub struct BevyInputHarness {
    app: App,
    server: LocalServer,
    client: GameInstanceManager,
    server_to_client: Receiver<ServerPacket>,
    /// The channel `forward_input` writes into. The harness drains it, records
    /// what it saw, and relays each event into the manager — a clean sniff
    /// point that avoids racing the manager on one MPMC channel.
    game_tap: Receiver<GameEventKind>,
    /// Count of `Key::KeyE` press edges `forward_input` emitted.
    e_forwarded: usize,
    /// Every `InputEvent::MouseMotion` `forward_input` emitted, in order.
    mouse_motions: Vec<(f32, f32)>,
    /// The stub primary-window entity carrying `Window` + `CursorOptions`.
    window: Entity,
}

impl BevyInputHarness {
    pub fn new() -> Self {
        game::set_hash_mode(game::HashMode::Full);

        // Manager input channel (manager reads game_recv; send_tick/push_game_event write game_send).
        let (game_send, game_recv) = unbounded::<GameEventKind>();
        // Manager -> bridge ClientUpdateEvent stream.
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // forward_input -> harness tap (relayed into the manager each step).
        let (tap_send, tap_recv) = unbounded::<GameEventKind>();

        let addr = "127.0.0.1:0".parse().unwrap();
        let mut client = GameInstanceManager::new(
            game_send,
            game_recv,
            bridge_send,
            addr,
            game::DebugFlags::default(),
        );
        // These tests drive input plumbing (key edge -> sim -> cursor grab), so
        // they buy the smallest world that still has a player in it:
        //
        // - LOD ring subscriptions OFF, for the same reason SimHarness::new
        //   turns them off: every requested key costs the inline LocalServer a
        //   synchronous build, and the production table's radius (32 regions)
        //   is thousands of keys — minutes of wall clock, gigabytes retained.
        // - Near window radius 0 (home region only, not the 3x3). Each region
        //   is a full worldgen + voxel-collider genesis on the server plus a
        //   deserialized copy on the client; eight neighbours no assertion
        //   looks at are ~9x the memory and time for nothing.
        client.set_lod_table(&[]);
        client.set_near_radius(0);

        let client_to_server = client.client_packet_recv();
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let server = LocalServer::new_with_generator(
            client_to_server,
            server_to_client_send,
            crate::local_server::lightweight_test_generator(),
        )
        .expect("local server");

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<bevy::input::mouse::AccumulatedMouseScroll>()
            .init_resource::<crate::renderer::input::MouseCoalesceWindow>()
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<UiUpdates>()
            .init_resource::<crate::renderer::collider_debug::DebugColliderLines>()
            .init_resource::<crate::renderer::lod_rings::LodRings>()
            .init_resource::<SnapshotVoxelQueue>()
            .init_resource::<RetiredRegionRoots>()
            .init_resource::<crate::renderer::cursor::CursorPolicy>()
            .insert_resource(ClientUpdates(bridge_recv))
            .insert_resource(GameEvents(tap_send))
            // Same ordering SimBridgePlugin uses: forward_input BEFORE the drains.
            .add_systems(
                PreUpdate,
                (
                    forward_input,
                    (
                        drain_client_updates,
                        despawn_retired_regions,
                        drain_region_updates,
                        apply_snapshot_voxels,
                        crate::renderer::cursor::apply_cursor_policy,
                    )
                        .chain(),
                )
                    .chain(),
            );

        // Stub primary window: all three components on one entity so both
        // `Query<&Window, With<PrimaryWindow>>` (forward_input) and
        // `Query<&mut CursorOptions, With<PrimaryWindow>>` (grab) match.
        let window = app
            .world_mut()
            .spawn((Window::default(), CursorOptions::default(), PrimaryWindow))
            .id();

        Self {
            app,
            server,
            client,
            server_to_client,
            game_tap: tap_recv,
            e_forwarded: 0,
            mouse_motions: Vec::new(),
            window,
        }
    }

    /// Handshake: request the player's region, then step until `LocalPlayer` is
    /// set, plus a few extra steps so the sim's `is_caught_up` gate opens.
    pub fn connect(&mut self) {
        self.client.start();
        for _ in 0..64 {
            self.step();
            if self.local_player().is_some() {
                break;
            }
        }
        for _ in 0..8 {
            self.step();
        }
    }

    /// One deterministic frame. `MinimalPlugins` has no `InputPlugin`, so
    /// `ButtonInput` is never auto-cleared: we clear the just_pressed/released
    /// edges ourselves after `forward_input` has consumed them, or they would
    /// re-fire every frame and toggle fps-cam every tick.
    pub fn step(&mut self) {
        // Front: forward_input pushes queued key edges to the tap; drains prior updates.
        self.app.update();
        self.drain_tap();
        self.clear_input_edges();

        // Sim lockstep (mirrors SimHarness::step). Relayed PlayerInput lands in
        // game_recv BEFORE this Tick, so the press is registered, then the Tick
        // runs the sim toggle in the same drain.
        self.client.send_tick();
        self.client
            .pump_packets(&self.server_to_client)
            .expect("client pump");
        self.server.pump().expect("server pump");
        self.server.tick();
        self.client
            .pump_packets(&self.server_to_client)
            .expect("client reconcile pump");

        // Back: drain this tick's region updates -> apply the cursor grab.
        self.app.update();
        self.clear_input_edges();
    }

    /// Relay every event `forward_input` produced into the manager, counting E
    /// press edges on the way through.
    fn drain_tap(&mut self) {
        while let Ok(ev) = self.game_tap.try_recv() {
            if let GameEventKind::PlayerInput(
                _,
                InputEvent::Key {
                    key: Key::KeyE,
                    pressed: true,
                },
            ) = ev
            {
                self.e_forwarded += 1;
            }
            if let GameEventKind::PlayerInput(_, InputEvent::MouseMotion { dx, dy }) = ev {
                self.mouse_motions.push((dx, dy));
            }
            let _ = self.client.push_game_event(ev);
        }
    }

    fn clear_input_edges(&mut self) {
        self.app
            .world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .clear();
        self.app
            .world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .clear();
        // Same reason: real Bevy's `InputPlugin` recomputes `AccumulatedMouseMotion`
        // every frame and `MinimalPlugins` has none, so an injected delta would
        // otherwise re-fire on every subsequent frame.
        *self
            .app
            .world_mut()
            .resource_mut::<AccumulatedMouseMotion>() = AccumulatedMouseMotion::default();
    }

    // --- input entry points ---

    pub fn press_key(&mut self, code: KeyCode) {
        self.app
            .world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(code);
    }
    pub fn release_key(&mut self, code: KeyCode) {
        self.app
            .world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(code);
    }
    pub fn press_e(&mut self) {
        self.press_key(KeyCode::KeyE);
    }
    pub fn release_e(&mut self) {
        self.release_key(KeyCode::KeyE);
    }

    /// Inject one frame's worth of raw mouse motion. Consumed by the next
    /// `step()` and cleared afterwards, so one injection == one frame.
    pub fn move_mouse(&mut self, delta: Vec2) {
        self.app
            .world_mut()
            .resource_mut::<AccumulatedMouseMotion>()
            .delta = delta;
    }

    /// Override the coalescing window. `Duration::ZERO` = emit every frame.
    pub fn set_mouse_coalesce_window(&mut self, window: Duration) {
        self.app
            .world_mut()
            .resource_mut::<crate::renderer::input::MouseCoalesceWindow>()
            .0 = window;
    }

    /// The harness's Bevy app (tests add the messages / resources they read).
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Push a sim event straight into the client manager (no Bevy input).
    pub fn push_sim_event(&mut self, ev: GameEventKind) {
        let _ = self.client.push_game_event(ev);
    }

    // --- inspectors (one per bug suspect) ---

    /// suspect (c): is `LocalPlayer` set?
    pub fn local_player(&self) -> Option<ClientId> {
        self.app.world().resource::<LocalPlayer>().0
    }

    /// Did `forward_input` emit the E press edge (cleared the LocalPlayer gate)?
    pub fn e_forwarded_count(&self) -> usize {
        self.e_forwarded
    }

    /// Every `MouseMotion` delta `forward_input` emitted, in order.
    pub fn mouse_motions(&self) -> &[(f32, f32)] {
        &self.mouse_motions
    }

    /// suspect (b): the local player's fps_cam_mode after the sim tick. `Some`
    /// once the home region is loaded.
    pub fn sim_fps_cam_mode(&self) -> Option<bool> {
        self.client.local_player_fps_cam_mode()
    }

    /// suspect (a) + end assertion: the stub window's live grab mode.
    pub fn cursor_grab_mode(&self) -> CursorGrabMode {
        self.app
            .world()
            .entity(self.window)
            .get::<CursorOptions>()
            .expect("primary window carries CursorOptions")
            .grab_mode
    }
}

impl Default for BevyInputHarness {
    fn default() -> Self {
        Self::new()
    }
}
