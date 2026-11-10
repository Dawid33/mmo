//! Render configuration shared by every world camera.

/// Native cameras use post-process antialiasing. SMAA is single-sample, so
/// leaving camera MSAA enabled would only add cost.
#[cfg(not(target_arch = "wasm32"))]
pub fn configure_native_antialiasing(
    mut commands: bevy::prelude::Commands,
    cameras: bevy::prelude::Query<
        (bevy::prelude::Entity, Option<&bevy::anti_alias::smaa::Smaa>),
        bevy::prelude::With<bevy::prelude::Camera3d>,
    >,
) {
    for (entity, smaa) in &cameras {
        let mut entity_commands = commands.entity(entity);
        entity_commands.insert(bevy::prelude::Msaa::Off);
        if smaa.is_none() {
            entity_commands.insert(bevy::anti_alias::smaa::Smaa::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_cameras_use_msaa_off_and_smaa() {
        use bevy::anti_alias::smaa::{Smaa, SmaaPreset};
        use bevy::prelude::*;

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_systems(Update, configure_native_antialiasing);
        let camera = app.world_mut().spawn(Camera3d::default()).id();
        let configured_camera = app
            .world_mut()
            .spawn((Camera3d::default(), Msaa::Sample4, Smaa::default()))
            .id();
        app.update();

        assert_eq!(*app.world().get::<Msaa>(camera).unwrap(), Msaa::Off);
        assert_eq!(
            *app.world().get::<Msaa>(configured_camera).unwrap(),
            Msaa::Off,
            "an existing SMAA component must not prevent MSAA from being disabled"
        );
        assert!(matches!(
            app.world().get::<Smaa>(camera).unwrap().preset,
            SmaaPreset::High
        ));
    }
}
