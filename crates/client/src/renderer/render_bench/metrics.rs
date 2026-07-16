use std::collections::BTreeMap;

use serde::Serialize;

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

#[derive(Default)]
pub struct PassSamples {
    suffix: &'static str,
    values: BTreeMap<String, Vec<f64>>,
}

impl PassSamples {
    pub fn with_suffix(suffix: &'static str) -> Self {
        Self {
            suffix,
            values: BTreeMap::new(),
        }
    }

    pub fn observe(&mut self, path: &str, value: f64) {
        let name = path.strip_suffix(self.suffix).unwrap_or(path);
        self.values.entry(name.to_owned()).or_default().push(value);
    }

    pub fn summaries(&self) -> BTreeMap<String, FloatSummary> {
        self.values
            .iter()
            .filter_map(|(name, values)| {
                FloatSummary::from_samples(values).map(|summary| (name.clone(), summary))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_summary_includes_range_percentiles_and_count() {
        let values = (1..=100).map(f64::from).collect::<Vec<_>>();
        assert_eq!(
            FloatSummary::from_samples(&values),
            Some(FloatSummary {
                min: 1.0,
                p50: 50.0,
                p95: 95.0,
                p99: 99.0,
                max: 100.0,
                sample_count: 100,
            })
        );
        assert_eq!(FloatSummary::from_samples(&[]), None);
    }

    #[test]
    fn count_summary_uses_the_same_nearest_rank_contract() {
        assert_eq!(
            CountSummary::from_samples(&[9, 1, 5, 3]),
            Some(CountSummary {
                min: 1,
                p50: 3,
                p95: 9,
                p99: 9,
                max: 9,
                sample_count: 4,
            })
        );
    }

    #[test]
    fn pass_samples_remove_only_the_metric_suffix() {
        let mut samples = PassSamples::with_suffix("/elapsed_gpu");
        samples.observe("render/main_opaque_pass_3d/elapsed_gpu", 2.5);
        samples.observe("render/main_opaque_pass_3d/elapsed_gpu", 3.5);
        assert_eq!(
            samples
                .summaries()
                .get("render/main_opaque_pass_3d")
                .expect("pass summary")
                .sample_count,
            2
        );
    }
}
