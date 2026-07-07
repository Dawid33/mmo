//! Headless Bevy-App harness driving the REAL client input pipeline:
//! `ButtonInput<KeyCode>` -> `forward_input` -> game channel ->
//! `GameInstanceManager` gates -> sim tick -> `SetFreeCam` ->
//! `drain_region_updates` -> `CursorGrabMode`. The Bevy-layer complement to
//! `SimHarness`: no window, no GPU, no threads, no wall clock. See
//! docs/superpowers/specs/2026-07-06-bevy-input-harness-design.md.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow, Window};
use crossbeam::channel::{unbounded, Receiver};
use game::{ClientId, ClientUpdateEvent, GameEventKind, InputEvent, Key, ServerPacket};

use crate::renderer::bridge::{drain_client_updates, drain_region_updates, Regions, RegionRoots, SimEntityMap};
use crate::renderer::hud::HudStatus;
use crate::renderer::input::forward_input;
use crate::renderer::ui_bridge::UiUpdates;
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
    /// The stub primary-window entity carrying `Window` + `CursorOptions`.
    window: Entity,
}

impl BevyInputHarness {
    pub fn new() -> Self {
        game::set_hash_verification(true);

        // Manager input channel (manager reads game_recv; send_tick/push_game_event write game_send).
        let (game_send, game_recv) = unbounded::<GameEventKind>();
        // Manager -> bridge ClientUpdateEvent stream.
        let (bridge_send, bridge_recv) = unbounded::<ClientUpdateEvent>();
        // forward_input -> harness tap (relayed into the manager each step).
        let (tap_send, tap_recv) = unbounded::<GameEventKind>();

        let addr = "127.0.0.1:0".parse().unwrap();
        let client = GameInstanceManager::new(game_send, game_recv, bridge_send, addr);

        let client_to_server = client.client_packet_recv();
        let (server_to_client_send, server_to_client) = unbounded::<ServerPacket>();
        let server = LocalServer::new(client_to_server, server_to_client_send).expect("local server");

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<LocalPlayer>()
            .init_resource::<Regions>()
            .init_resource::<RegionRoots>()
            .init_resource::<SimEntityMap>()
            .init_resource::<HudStatus>()
            .init_resource::<UiUpdates>()
            .insert_resource(ClientUpdates(bridge_recv))
            .insert_resource(GameEvents(tap_send))
            // Same ordering SimBridgePlugin uses: forward_input BEFORE the drains.
            .add_systems(
                PreUpdate,
                (forward_input, (drain_client_updates, drain_region_updates).chain()).chain(),
            );

        // Stub primary window: all three components on one entity so both
        // `Query<&Window, With<PrimaryWindow>>` (forward_input) and
        // `Query<&mut CursorOptions, With<PrimaryWindow>>` (grab) match.
        let window = app
            .world_mut()
            .spawn((Window::default(), CursorOptions::default(), PrimaryWindow))
            .id();

        Self { app, server, client, server_to_client, game_tap: tap_recv, e_forwarded: 0, window }
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
        self.client.pump(&self.server_to_client).expect("client pump");
        self.server.pump().expect("server pump");
        self.server.tick();
        self.client.pump(&self.server_to_client).expect("client reconcile pump");

        // Back: drain this tick's region updates -> apply the cursor grab.
        self.app.update();
        self.clear_input_edges();
    }

    /// Relay every event `forward_input` produced into the manager, counting E
    /// press edges on the way through.
    fn drain_tap(&mut self) {
        while let Ok(ev) = self.game_tap.try_recv() {
            if let GameEventKind::PlayerInput(_, InputEvent::Key { key: Key::KeyE, pressed: true }) = ev {
                self.e_forwarded += 1;
            }
            let _ = self.client.push_game_event(ev);
        }
    }

    fn clear_input_edges(&mut self) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
        self.app.world_mut().resource_mut::<ButtonInput<MouseButton>>().clear();
    }

    // --- input entry points ---

    pub fn press_key(&mut self, code: KeyCode) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(code);
    }
    pub fn release_key(&mut self, code: KeyCode) {
        self.app.world_mut().resource_mut::<ButtonInput<KeyCode>>().release(code);
    }
    pub fn press_e(&mut self) {
        self.press_key(KeyCode::KeyE);
    }
    pub fn release_e(&mut self) {
        self.release_key(KeyCode::KeyE);
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
