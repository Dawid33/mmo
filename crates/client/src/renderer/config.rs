use std::fmt;

use bevy::prelude::Resource;

/// Full-resolution terrain rendering strategy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NearRenderBackend {
    /// One Bevy `Mesh` asset and draw item per chunk.
    PerChunk,
    /// Shared quad-record buffers with one direct draw per visible chunk.
    GroupedDirect,
    /// GPU-built visibility and indirect draw commands over shared records.
    GpuDriven,
}

impl NearRenderBackend {
    /// Both modern near paths consume the same packed quad records. This keeps
    /// meshing independent of whether draw submission is direct or GPU-driven.
    pub const fn uses_quad_records(self) -> bool {
        !matches!(self, Self::PerChunk)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PerChunk => "per-chunk",
            Self::GroupedDirect => "direct",
            Self::GpuDriven => "gpu",
        }
    }
}

/// Backend implementations compiled into this client build.
///
/// Auto-selection is deliberately driven by this value rather than platform
/// names scattered through render systems. Future backends can flip their
/// gate when their complete pipeline is registered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainRenderCapabilities {
    pub grouped_direct: bool,
    pub gpu_driven: bool,
}

impl TerrainRenderCapabilities {
    pub const fn compiled() -> Self {
        Self {
            grouped_direct: true,
            // The render-world capability resolver still falls back to
            // grouped-direct when an adapter or pipeline misses a hard gate.
            gpu_driven: true,
        }
    }
}

/// The resolved near renderer, shared by meshing and render setup. Far
/// terrain (the LOD rings) is always rasterized.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerrainRenderConfig {
    pub near: NearRenderBackend,
}

impl TerrainRenderConfig {
    /// Resolve an optional environment value for a specified target/capability
    /// set. The pure inputs keep platform and fallback behavior testable.
    pub fn resolve(
        near: Option<&str>,
        is_wasm: bool,
        capabilities: TerrainRenderCapabilities,
    ) -> Result<Self, TerrainRenderConfigError> {
        if is_wasm {
            // Grouped-direct is what a WebGPU browser build can run; the
            // `webgl2` build has no storage buffers at all, and
            // `for_device` drops it to per-chunk once the adapter is known.
            return Ok(Self {
                near: NearRenderBackend::GroupedDirect,
            });
        }

        let near = match near {
            Some(value) => parse_near_backend(value)?,
            None if capabilities.gpu_driven => NearRenderBackend::GpuDriven,
            None if capabilities.grouped_direct => NearRenderBackend::GroupedDirect,
            None => NearRenderBackend::PerChunk,
        };
        Ok(Self { near })
    }

    /// Reconcile the requested backend with what the adapter can run. Both
    /// quad-record backends read their records from storage buffers in the
    /// VERTEX stage; a device without that (WebGL2, GLES 3.0) can only draw
    /// per-chunk meshes. Must be applied before any chunk is meshed, because
    /// the backend decides whether meshing emits `ChunkQuads` or `Mesh3d`.
    pub const fn for_device(self, vertex_storage_buffers: bool) -> Self {
        if self.near.uses_quad_records() && !vertex_storage_buffers {
            Self {
                near: NearRenderBackend::PerChunk,
            }
        } else {
            self
        }
    }

    /// Resolve the process environment on native. Browser builds intentionally
    /// never read overrides and always return the portable backend.
    pub fn from_env() -> Result<Self, TerrainRenderConfigError> {
        #[cfg(target_arch = "wasm32")]
        {
            Self::resolve(None, true, TerrainRenderCapabilities::compiled())
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let near = std::env::var("VOXEL_NEAR_RENDER").ok();
            Self::resolve(near.as_deref(), false, TerrainRenderCapabilities::compiled())
        }
    }
}

impl Default for TerrainRenderConfig {
    fn default() -> Self {
        Self::from_env().unwrap_or_else(|error| panic!("{error}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerrainRenderConfigError {
    InvalidNear(String),
}

impl fmt::Display for TerrainRenderConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNear(value) => write!(
                f,
                "invalid VOXEL_NEAR_RENDER value `{value}` (expected per-chunk, direct, or gpu)"
            ),
        }
    }
}

impl std::error::Error for TerrainRenderConfigError {}

pub fn parse_near_backend(value: &str) -> Result<NearRenderBackend, TerrainRenderConfigError> {
    match value {
        "per-chunk" => Ok(NearRenderBackend::PerChunk),
        "direct" => Ok(NearRenderBackend::GroupedDirect),
        "gpu" => Ok(NearRenderBackend::GpuDriven),
        value => Err(TerrainRenderConfigError::InvalidNear(value.to_owned())),
    }
}

/// Native cameras use the same post-process antialiasing in interactive and
/// benchmark runs. SMAA is single-sample, so leaving camera MSAA enabled would
/// add cost and make backend comparisons inconsistent.
#[cfg(not(target_arch = "wasm32"))]
pub fn configure_native_antialiasing(
    mut commands: bevy::prelude::Commands,
    cameras: bevy::prelude::Query<
        (bevy::prelude::Entity, Option<&bevy::anti_alias::smaa::Smaa>),
        bevy::prelude::With<bevy::prelude::Camera3d>,
    >,
) {
    for (entity, smaa) in &cameras {
        // `try_`: a session camera (the first-person eyes) can be despawned by
        // `session::end_session` before these commands are applied.
        let mut entity_commands = commands.entity(entity);
        entity_commands.try_insert(bevy::prelude::Msaa::Off);
        if smaa.is_none() {
            entity_commands.try_insert(bevy::anti_alias::smaa::Smaa::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: TerrainRenderCapabilities = TerrainRenderCapabilities {
        grouped_direct: true,
        gpu_driven: false,
    };

    #[test]
    fn parses_all_documented_backend_values() {
        assert_eq!(
            parse_near_backend("per-chunk"),
            Ok(NearRenderBackend::PerChunk)
        );
        assert_eq!(
            parse_near_backend("direct"),
            Ok(NearRenderBackend::GroupedDirect)
        );
        assert_eq!(parse_near_backend("gpu"), Ok(NearRenderBackend::GpuDriven));
    }

    #[test]
    fn native_auto_chooses_best_available_backends() {
        assert_eq!(
            TerrainRenderConfig::resolve(None, false, CURRENT).unwrap(),
            TerrainRenderConfig {
                near: NearRenderBackend::GroupedDirect,
            }
        );
        assert_eq!(
            TerrainRenderConfig::resolve(
                None,
                false,
                TerrainRenderCapabilities {
                    grouped_direct: true,
                    gpu_driven: true,
                },
            )
            .unwrap(),
            TerrainRenderConfig {
                near: NearRenderBackend::GpuDriven,
            }
        );
    }

    #[test]
    fn compiled_native_auto_enables_gpu_near() {
        let capabilities = TerrainRenderCapabilities::compiled();

        assert!(capabilities.gpu_driven);
        assert_eq!(
            TerrainRenderConfig::resolve(None, false, capabilities).unwrap(),
            TerrainRenderConfig {
                near: NearRenderBackend::GpuDriven,
            }
        );
    }

    #[test]
    fn wasm_ignores_overrides_and_uses_portable_backends() {
        assert_eq!(
            TerrainRenderConfig::resolve(Some("gpu"), true, CURRENT).unwrap(),
            TerrainRenderConfig {
                near: NearRenderBackend::GroupedDirect,
            }
        );
    }

    #[test]
    fn device_without_vertex_storage_falls_back_to_per_chunk() {
        for near in [NearRenderBackend::GroupedDirect, NearRenderBackend::GpuDriven] {
            let config = TerrainRenderConfig { near };
            assert_eq!(config.for_device(true), config);
            assert_eq!(
                config.for_device(false),
                TerrainRenderConfig {
                    near: NearRenderBackend::PerChunk,
                }
            );
        }
        let per_chunk = TerrainRenderConfig {
            near: NearRenderBackend::PerChunk,
        };
        assert_eq!(per_chunk.for_device(false), per_chunk);
    }

    #[test]
    fn invalid_values_name_the_environment_variable() {
        assert_eq!(
            TerrainRenderConfig::resolve(Some("meshlets"), false, CURRENT)
                .unwrap_err()
                .to_string(),
            "invalid VOXEL_NEAR_RENDER value `meshlets` (expected per-chunk, direct, or gpu)"
        );
    }

    #[test]
    fn only_per_chunk_uses_mesh_assets() {
        assert!(!NearRenderBackend::PerChunk.uses_quad_records());
        assert!(NearRenderBackend::GroupedDirect.uses_quad_records());
        assert!(NearRenderBackend::GpuDriven.uses_quad_records());
    }

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
