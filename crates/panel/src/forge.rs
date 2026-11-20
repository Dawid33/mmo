//! A forge as a Kubernetes Job: `worldctl forge --db $(VOXEL_WORLD_DB) …` in the
//! world's namespace, on the running release's server image (the toolbox's), with
//! the database URL from Secret `world-db` — never in the Job's spec: Kubernetes
//! expands `$(VOXEL_WORLD_DB)` when the container starts.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::world::FORGE_COMPONENT;

/// The generator's presets (`worldgen::forge::presets::PRESETS`, by name). Kept here so
/// the panel does not build the game; tests/it.rs `presets` fails when they drift.
pub const PRESETS: &[&str] = &[
    "Earth-like",
    "Great ranges",
    "Highlands",
    "Lowlands",
    "Dry",
    "Wet",
];

/// worldctl's world sizes (`worldgen::forge::play::WORLD_SIZES` and `DEV_WORLD_SIZE`,
/// by name; `worldctl forge --size <name>`). Kept here so the panel does not build the game.
pub const SIZES: &[&str] = &["Tiny", "Small", "Medium", "Large", "Dev"];

pub const DEFAULT_MEMORY_MI: u32 = 1024;
const MEMORY_MI: std::ops::RangeInclusive<u32> = 256..=16_384;
const CELL_M: std::ops::RangeInclusive<u32> = 4..=256;
/// A forge Job that runs longer than this is killed.
const DEADLINE_S: u64 = 3600;
/// A finished forge Job (and its pod's log) is kept this long.
const TTL_S: u64 = 7 * 86_400;
const UID: u64 = 10001;

/// The forge form as posted; empty fields mean worldctl's default.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ForgeForm {
    #[serde(default)]
    pub preset: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub cell_m: String,
    #[serde(default)]
    pub seed: String,
    #[serde(default)]
    pub memory_mi: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForgeRequest {
    pub preset: Option<&'static str>,
    /// One of [`SIZES`].
    pub size: Option<&'static str>,
    pub cell_m: Option<u32>,
    pub seed: Option<u64>,
    pub memory_mi: u32,
}

impl ForgeForm {
    pub fn parse(&self) -> Result<ForgeRequest, String> {
        let preset = match self.preset.trim() {
            "" => None,
            p => Some(
                *PRESETS
                    .iter()
                    .find(|n| n.eq_ignore_ascii_case(p))
                    .ok_or_else(|| format!("no preset named {p:?}"))?,
            ),
        };
        let size = match self.size.trim() {
            "" => None,
            v => Some(
                *SIZES
                    .iter()
                    .find(|n| n.eq_ignore_ascii_case(v))
                    .ok_or_else(|| format!("no world size named {v:?} ({})", SIZES.join(", ")))?,
            ),
        };
        let ranged = |field: &str,
                      v: &str,
                      range: std::ops::RangeInclusive<u32>|
         -> Result<Option<u32>, String> {
            match v.trim() {
                "" => Ok(None),
                v => v
                    .parse::<u32>()
                    .ok()
                    .filter(|n| range.contains(n))
                    .map(Some)
                    .ok_or_else(|| {
                        format!(
                            "{field}: a whole number in {}..={}",
                            range.start(),
                            range.end()
                        )
                    }),
            }
        };
        let seed = match self.seed.trim() {
            "" => None,
            s => {
                let hex = s.trim_start_matches("0x").replace('_', "");
                Some(
                    u64::from_str_radix(&hex, 16)
                        .map_err(|_| format!("seed {s:?}: not a hex number (up to 16 digits)"))?,
                )
            }
        };
        Ok(ForgeRequest {
            preset,
            size,
            cell_m: ranged("cell", &self.cell_m, CELL_M)?,
            seed,
            memory_mi: ranged("memory", &self.memory_mi, MEMORY_MI)?.unwrap_or(DEFAULT_MEMORY_MI),
        })
    }
}

impl ForgeRequest {
    /// worldctl's arguments (after the program name).
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = ["forge", "--db", "$(VOXEL_WORLD_DB)"]
            .map(String::from)
            .to_vec();
        if let Some(p) = self.preset {
            a.extend(["--preset".into(), p.into()]);
        }
        if let Some(s) = self.size {
            a.extend(["--size".into(), s.to_ascii_lowercase()]);
        }
        if let Some(c) = self.cell_m {
            a.extend(["--cell".into(), c.to_string()]);
        }
        if let Some(s) = self.seed {
            a.extend(["--seed".into(), format!("{s:x}")]);
        }
        a
    }

    /// The Job object. `image` and `pull_secrets` (a `imagePullSecrets` array) come from
    /// the world's toolbox; `world_id` labels it as the module labels its pods.
    pub fn job(&self, name: &str, world_id: &str, image: &str, pull_secrets: &Value) -> Value {
        let labels = json!({
            "app.kubernetes.io/name": "world",
            "app.kubernetes.io/instance": world_id,
            "app.kubernetes.io/component": FORGE_COMPONENT,
            "app.kubernetes.io/managed-by": "panel",
        });
        let memory = format!("{}Mi", self.memory_mi);
        json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": { "name": name, "labels": labels },
            "spec": {
                "backoffLimit": 0,
                "activeDeadlineSeconds": DEADLINE_S,
                "ttlSecondsAfterFinished": TTL_S,
                "template": {
                    "metadata": { "labels": labels },
                    "spec": {
                        "restartPolicy": "Never",
                        "automountServiceAccountToken": false,
                        "enableServiceLinks": false,
                        "imagePullSecrets": pull_secrets,
                        "securityContext": { "runAsUser": UID, "runAsGroup": UID, "runAsNonRoot": true, "fsGroup": UID },
                        "containers": [{
                            "name": "forge",
                            "image": image,
                            "imagePullPolicy": "IfNotPresent",
                            "command": ["worldctl"],
                            "args": self.args(),
                            "env": [{
                                "name": "VOXEL_WORLD_DB",
                                "valueFrom": { "secretKeyRef": { "name": "world-db", "key": "url" } }
                            }],
                            "securityContext": { "allowPrivilegeEscalation": false, "capabilities": { "drop": ["ALL"] } },
                            "resources": {
                                "requests": { "cpu": "100m", "memory": "256Mi" },
                                "limits": { "memory": memory }
                            }
                        }]
                    }
                }
            }
        })
    }
}
