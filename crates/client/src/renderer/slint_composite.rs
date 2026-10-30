//! Slint layers on screen: the camera they are composited onto.
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;

/// The camera the UI is drawn over — the local player's active camera (moved
/// by `bridge::enforce_single_active_camera`) or the commander camera. At most
/// one is active at a time.
#[derive(Component, Clone, Copy, Default, Debug, ExtractComponent)]
pub struct UiCamera;
