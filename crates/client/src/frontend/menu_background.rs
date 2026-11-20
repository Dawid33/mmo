//! The main menu's backdrop: the map preview (`preview.png`) of one saved
//! world, picked at random each time the menu is entered.
//!
//! Previews are as large as the world's raster (6272² px for a 100 km world at
//! 16 m), so the pick is decoded and shrunk to [`BACKGROUND_MAX_PX`] on a
//! background thread; the menu shows the plain backdrop until it arrives. The
//! last decoded picture is kept and re-used while its world still exists and
//! is the one picked again, and a finished decode for an older pick is dropped.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use crossbeam::channel::{self, Receiver};
use slint_ui::FrontendUi;
use slint_ui::slint::{Image as SlintImage, Rgba8Pixel, SharedPixelBuffer};
use worldgen::{forged, world_store};

use crate::renderer::slint_overlay::SlintLayer;
use crate::session::AppState;

/// Longest edge of the decoded backdrop: enough to cover a 4K-wide window at
/// `image-fit: cover` without visible softening on common screens.
pub const BACKGROUND_MAX_PX: u32 = 2048;

type Decoded = (PathBuf, Option<SharedPixelBuffer<Rgba8Pixel>>);

#[derive(Resource, Default)]
pub struct MenuBackground {
    /// The preview being decoded and where its result arrives.
    pending: Option<(PathBuf, Receiver<Decoded>)>,
    /// The preview shown (its path, the picture).
    shown: Option<(PathBuf, SharedPixelBuffer<Rgba8Pixel>)>,
    /// The picture changed since the last push to the UI.
    dirty: bool,
}

pub struct MenuBackgroundPlugin;

impl Plugin for MenuBackgroundPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MenuBackground>()
            .add_systems(OnEnter(AppState::MainMenu), pick_background)
            .add_systems(Update, (receive_background, push_background).chain());
    }
}

/// A seed that differs per call, for the pick (no RNG state to keep).
fn pick_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // splitmix64 finaliser, so consecutive calls spread over the list.
    let mut z = (nanos as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The preview to show from `previews`, by `seed` (`None`: no saved world has one).
pub fn choose(previews: &[PathBuf], seed: u64) -> Option<&PathBuf> {
    (!previews.is_empty()).then(|| &previews[(seed % previews.len() as u64) as usize])
}

fn decode(path: &Path) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let image = image::open(path).ok()?;
    let image = if image.width().max(image.height()) > BACKGROUND_MAX_PX {
        image.thumbnail(BACKGROUND_MAX_PX, BACKGROUND_MAX_PX)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

fn pick_background(mut bg: ResMut<MenuBackground>) {
    let root = forged::from_repo_root(Path::new(world_store::WORLDS_DIR));
    let previews: Vec<PathBuf> = world_store::list(&root)
        .into_iter()
        .filter_map(|e| e.preview)
        .collect();
    let Some(path) = choose(&previews, pick_seed()).cloned() else {
        if bg.shown.take().is_some() {
            bg.dirty = true;
        }
        bg.pending = None;
        return;
    };
    if bg.shown.as_ref().is_some_and(|(p, _)| *p == path) {
        return;
    }
    let (tx, rx) = channel::bounded(1);
    let job = path.clone();
    let spawned = std::thread::Builder::new()
        .name("menu-background".into())
        .spawn(move || {
            let picture = decode(&job);
            let _ = tx.send((job, picture));
        });
    match spawned {
        Ok(_) => bg.pending = Some((path, rx)),
        Err(e) => warn!("menu background: could not spawn the decode thread: {e}"),
    }
}

fn receive_background(mut bg: ResMut<MenuBackground>) {
    let Some((want, rx)) = &bg.pending else {
        return;
    };
    let Ok((path, picture)) = rx.try_recv() else {
        return;
    };
    if path != *want {
        return;
    }
    bg.pending = None;
    match picture {
        Some(picture) => {
            bg.shown = Some((path, picture));
            bg.dirty = true;
        }
        None => warn!("menu background: could not decode {}", path.display()),
    }
}

fn push_background(mut bg: ResMut<MenuBackground>, layer: Option<NonSend<SlintLayer<FrontendUi>>>) {
    let Some(layer) = layer else { return };
    if !bg.dirty {
        return;
    }
    bg.dirty = false;
    let image = bg
        .shown
        .as_ref()
        .map(|(_, p)| SlintImage::from_rgba8(p.clone()))
        .unwrap_or_default();
    layer.overlay.ui().set_menu_background(image);
}
