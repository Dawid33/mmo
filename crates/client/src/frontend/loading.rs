//! The loading screen: an opaque cover (`FrontendUi` screen 4, `loading.slint`)
//! over the world from the moment the player starts play — or plays a region,
//! or starts straight in the world — until the commander camera has been put
//! over the body ([`CommanderCamera::placed`]) and the home region's terrain
//! has been meshed, then faded out over [`LOADING_FADE_SECS`]. The world loads
//! (and renders) behind it as usual, so the camera, meshing order and pipelines
//! are all warm when it lifts; only the far LOD rings may still be streaming.
//!
//! Only the commander view gets one: the first-person dev modes (render
//! benchmark, autopilot) keep their own readiness gates.

use bevy::prelude::*;

use crate::renderer::bridge::{PendingSnapshotVoxels, RegionRoots, SnapshotVoxelTask};
use crate::renderer::commander::{CameraMode, CommanderCamera};
use crate::renderer::hud::HudStatus;
use crate::renderer::meshing::chunk_mesh::ChunkMeshDirty;
use crate::renderer::meshing::queue::RemeshQueue;
use crate::renderer::meshing::MeshingTask;
use crate::session::{AppState, InPlay};

/// Consecutive frames the home region must have no meshing work left (with
/// the camera placed) before the cover lifts: snapshot batches and brick
/// derivation arrive in bursts with idle frames between them.
pub const LOADING_STABLE_FRAMES: u32 = 10;
/// How long the cover takes to fade out.
pub const LOADING_FADE_SECS: f32 = 0.5;
/// The cover lifts after this long whatever the state, so a load that never
/// settles (or a body that never shows) cannot hide the world for good.
pub const LOADING_TIMEOUT_SECS: f32 = 90.0;
/// Bar position once the home region's terrain has arrived and meshing begins;
/// the meshing fills the rest.
pub const LOADING_TERRAIN_ARRIVED: f32 = 0.15;

pub const STEP_JOINING: &str = "Joining the world\u{2026}";
pub const STEP_RECEIVING: &str = "Receiving terrain\u{2026}";
pub const STEP_BUILDING: &str = "Building terrain\u{2026}";
pub const STEP_PLACING: &str = "Finding your dwarves\u{2026}";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CoverPhase {
    /// Nothing drawn.
    #[default]
    Clear,
    /// Opaque, loading; `since` = real seconds when it went up.
    Covering { since: f32 },
    /// Fading out since `since`.
    Fading { since: f32 },
}

/// The loading screen's state. Reset with the session.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct LoadingCover {
    pub phase: CoverPhase,
    /// The bar, 0..1, never moving back while one load lasts.
    pub progress: f32,
    pub step: &'static str,
    /// Most meshing work outstanding in the home region during this load
    /// (the bar's denominator).
    peak_outstanding: usize,
    stable_frames: u32,
}

/// What the cover decides on, gathered each frame (pure data, for tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadSignals {
    /// In the world, in play and on the commander view: the cover may show.
    pub eligible: bool,
    pub camera_placed: bool,
    pub home_known: bool,
    /// The home region's render root exists (its snapshot has arrived).
    pub home_rooted: bool,
    /// Meshing work left in the home region: queued and in-flight bricks,
    /// chunks awaiting their bricks or their merged mesh.
    pub outstanding: usize,
}

impl LoadingCover {
    /// Whether anything is drawn.
    pub fn visible(&self) -> bool {
        self.phase != CoverPhase::Clear
    }

    /// Opacity at real time `now`.
    pub fn fade(&self, now: f32) -> f32 {
        match self.phase {
            CoverPhase::Clear => 0.0,
            CoverPhase::Covering { .. } => 1.0,
            CoverPhase::Fading { since } => {
                let t = ((now - since) / LOADING_FADE_SECS).clamp(0.0, 1.0);
                // Smoothstep down from 1.
                1.0 - t * t * (3.0 - 2.0 * t)
            }
        }
    }

    /// Advance one frame at real time `now`. Returns `Some(timed_out)` on the
    /// frame the cover starts lifting.
    pub fn step(&mut self, s: LoadSignals, now: f32) -> Option<bool> {
        if !s.eligible {
            *self = Self::default();
            return None;
        }
        // A camera that is not (or no longer) over the body means a load is on:
        // the first placement, or a Play into another region.
        if !s.camera_placed && !matches!(self.phase, CoverPhase::Covering { .. }) {
            *self = Self { phase: CoverPhase::Covering { since: now }, step: STEP_JOINING, ..Self::default() };
        }
        match self.phase {
            CoverPhase::Clear => None,
            CoverPhase::Fading { since } => {
                if now - since >= LOADING_FADE_SECS {
                    self.phase = CoverPhase::Clear;
                }
                None
            }
            CoverPhase::Covering { since } => {
                self.peak_outstanding = self.peak_outstanding.max(s.outstanding);
                let (step, progress) = if !s.home_known {
                    (STEP_JOINING, 0.0)
                } else if !s.home_rooted || (s.outstanding == 0 && self.peak_outstanding == 0) {
                    (STEP_RECEIVING, LOADING_TERRAIN_ARRIVED * 0.5)
                } else if s.outstanding > 0 {
                    let done = 1.0 - s.outstanding as f32 / self.peak_outstanding as f32;
                    (STEP_BUILDING, LOADING_TERRAIN_ARRIVED + (0.98 - LOADING_TERRAIN_ARRIVED) * done)
                } else if !s.camera_placed {
                    (STEP_PLACING, 0.98)
                } else {
                    (STEP_BUILDING, 1.0)
                };
                self.step = step;
                self.progress = self.progress.max(progress);

                let settled = s.camera_placed && s.home_rooted && s.outstanding == 0;
                self.stable_frames = if settled { self.stable_frames + 1 } else { 0 };
                let timed_out = now - since >= LOADING_TIMEOUT_SECS;
                if self.stable_frames >= LOADING_STABLE_FRAMES || timed_out {
                    self.progress = 1.0;
                    self.phase = CoverPhase::Fading { since: now };
                    return Some(timed_out && self.stable_frames < LOADING_STABLE_FRAMES);
                }
                None
            }
        }
    }
}

/// Gathers [`LoadSignals`] and advances the [`LoadingCover`]. Runs in
/// `PostUpdate`, after every `Update` system that starts play or re-arms the
/// camera, so the cover is up on the very frame the world camera takes over.
#[allow(clippy::too_many_arguments)] // Bevy system: params are DI, not an API
pub fn update_loading_cover(
    mut cover: ResMut<LoadingCover>,
    policy: Option<ResMut<crate::renderer::cursor::CursorPolicy>>,
    time: Res<Time<Real>>,
    state: Option<Res<State<AppState>>>,
    in_play: Option<Res<InPlay>>,
    mode: Option<Res<CameraMode>>,
    cam: Option<Res<CommanderCamera>>,
    status: Option<Res<HudStatus>>,
    roots: Option<Res<RegionRoots>>,
    queue: Option<Res<RemeshQueue>>,
    parents: Query<&ChildOf>,
    in_flight: Query<
        &ChildOf,
        Or<(With<MeshingTask>, With<PendingSnapshotVoxels>, With<SnapshotVoxelTask>, With<ChunkMeshDirty>)>,
    >,
) {
    let eligible = state.is_some_and(|s| *s.get() == AppState::InWorld)
        && in_play.is_some_and(|e| e.0)
        && mode.is_some_and(|m| *m == CameraMode::Commander)
        && cam.is_some();
    let home = status.and_then(|s| s.home_region);
    let root = home.and_then(|rc| roots.as_ref().and_then(|r| r.0.get(&rc).copied()));
    // Counted only while a load is on: steady play pays nothing here.
    let loading = matches!(cover.phase, CoverPhase::Covering { .. }) || cam.as_ref().is_some_and(|c| !c.placed);
    let outstanding = match root {
        Some(root) if eligible && loading => {
            let queued = queue.as_ref().map_or(0, |q| {
                q.dirty.keys().filter(|e| parents.get(**e).is_ok_and(|p| p.parent() == root)).count()
            });
            queued + in_flight.iter().filter(|p| p.parent() == root).count()
        }
        _ => 0,
    };
    let signals = LoadSignals {
        eligible,
        camera_placed: cam.is_some_and(|c| c.placed),
        home_known: home.is_some(),
        home_rooted: root.is_some(),
        outstanding,
    };
    let now = time.elapsed_secs();
    let since = match cover.phase {
        CoverPhase::Covering { since } => Some(since),
        _ => None,
    };
    if let Some(timed_out) = cover.step(signals, now) {
        let secs = since.map_or(0.0, |s| now - s);
        if timed_out {
            warn!("[loading] the world did not settle in {LOADING_TIMEOUT_SECS} s; lifting the loading screen anyway");
        }
        game::perf_event!("loading_screen_lifted", "secs={secs:.2} timed_out={timed_out}");
    }
    // Until the cover is gone (faded out too) the player's input goes nowhere
    // and the pointer stays free (`CursorPolicy::loading`).
    if let Some(mut policy) = policy {
        let loading = cover.phase != CoverPhase::Clear;
        if policy.loading != loading {
            policy.loading = loading;
        }
    }
}

