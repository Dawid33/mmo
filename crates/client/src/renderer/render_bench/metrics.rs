use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PassMetric {
    ElapsedCpu,
    ElapsedGpu,
    VertexShaderInvocations,
    ClipperInvocations,
    ClipperPrimitivesOut,
    FragmentShaderInvocations,
    ComputeShaderInvocations,
}

pub fn classify(path: &str) -> Option<(&str, PassMetric)> {
    const SUFFIXES: [(&str, PassMetric); 7] = [
        ("/elapsed_cpu", PassMetric::ElapsedCpu),
        ("/elapsed_gpu", PassMetric::ElapsedGpu),
        (
            "/vertex_shader_invocations",
            PassMetric::VertexShaderInvocations,
        ),
        ("/clipper_invocations", PassMetric::ClipperInvocations),
        ("/clipper_primitives_out", PassMetric::ClipperPrimitivesOut),
        (
            "/fragment_shader_invocations",
            PassMetric::FragmentShaderInvocations,
        ),
        (
            "/compute_shader_invocations",
            PassMetric::ComputeShaderInvocations,
        ),
    ];
    SUFFIXES
        .into_iter()
        .find_map(|(suffix, metric)| path.strip_suffix(suffix).map(|name| (name, metric)))
}

#[derive(Default)]
pub struct RenderDiagnosticSamples {
    pub elapsed_cpu: BTreeMap<String, Vec<f64>>,
    pub elapsed_gpu: BTreeMap<String, Vec<f64>>,
    pub vertex_shader_invocations: BTreeMap<String, Vec<u64>>,
    pub clipper_invocations: BTreeMap<String, Vec<u64>>,
    pub clipper_primitives_out: BTreeMap<String, Vec<u64>>,
    pub fragment_shader_invocations: BTreeMap<String, Vec<u64>>,
    pub compute_shader_invocations: BTreeMap<String, Vec<u64>>,
}

impl RenderDiagnosticSamples {
    pub fn observe(&mut self, path: &str, value: f64) {
        if !value.is_finite() || value < 0.0 {
            return;
        }
        let Some((name, metric)) = classify(path) else {
            return;
        };
        match metric {
            PassMetric::ElapsedCpu => self
                .elapsed_cpu
                .entry(name.to_owned())
                .or_default()
                .push(value),
            PassMetric::ElapsedGpu => self
                .elapsed_gpu
                .entry(name.to_owned())
                .or_default()
                .push(value),
            PassMetric::VertexShaderInvocations => {
                push_count(&mut self.vertex_shader_invocations, name, value)
            }
            PassMetric::ClipperInvocations => {
                push_count(&mut self.clipper_invocations, name, value)
            }
            PassMetric::ClipperPrimitivesOut => {
                push_count(&mut self.clipper_primitives_out, name, value)
            }
            PassMetric::FragmentShaderInvocations => {
                push_count(&mut self.fragment_shader_invocations, name, value)
            }
            PassMetric::ComputeShaderInvocations => {
                push_count(&mut self.compute_shader_invocations, name, value)
            }
        }
    }

    pub fn float_summaries(values: &BTreeMap<String, Vec<f64>>) -> BTreeMap<String, FloatSummary> {
        values
            .iter()
            .filter_map(|(name, values)| {
                FloatSummary::from_samples(values).map(|summary| (name.clone(), summary))
            })
            .collect()
    }

    pub fn count_summaries(values: &BTreeMap<String, Vec<u64>>) -> BTreeMap<String, CountSummary> {
        values
            .iter()
            .filter_map(|(name, values)| {
                CountSummary::from_samples(values).map(|summary| (name.clone(), summary))
            })
            .collect()
    }
}

fn push_count(target: &mut BTreeMap<String, Vec<u64>>, name: &str, value: f64) {
    let count = if value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value as u64
    };
    target.entry(name.to_owned()).or_default().push(count);
}

/// Bevy diagnostic paths do not encode span containment. Returning an estimate
/// would risk adding a parent pass and its nested children more than once.
pub fn gpu_busy_estimate_ms(_elapsed_gpu: &BTreeMap<String, Vec<f64>>) -> Option<FloatSummary> {
    None
}

fn nearest_rank_index(len: usize, percent: usize) -> usize {
    (percent * len).div_ceil(100).max(1) - 1
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FloatSummary {
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub sample_count: usize,
}

impl FloatSummary {
    pub fn from_samples(samples: &[f64]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(f64::total_cmp);
        Some(Self {
            min: sorted[0],
            p50: sorted[nearest_rank_index(sorted.len(), 50)],
            p95: sorted[nearest_rank_index(sorted.len(), 95)],
            p99: sorted[nearest_rank_index(sorted.len(), 99)],
            max: *sorted.last().expect("non-empty checked above"),
            sample_count: sorted.len(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CountSummary {
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
    pub sample_count: usize,
}

impl CountSummary {
    pub fn from_samples(samples: &[u64]) -> Option<Self> {
        let mut sorted = samples.to_vec();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_unstable();
        Some(Self {
            min: sorted[0],
            p50: sorted[nearest_rank_index(sorted.len(), 50)],
            p95: sorted[nearest_rank_index(sorted.len(), 95)],
            p99: sorted[nearest_rank_index(sorted.len(), 99)],
            max: *sorted.last().expect("non-empty checked above"),
            sample_count: sorted.len(),
        })
    }
}
