//! The `mmo_*` series of a coordinator's or host's `/metrics` (`server/src/health.rs`)
//! the panel shows. A plain reader of the Prometheus text format: `name{labels} value`.

/// What one pod's `/metrics` says. `None` = the series was absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PodMetrics {
    pub registered: Option<bool>,
    /// The `why` of the `mmo_waiting` series at 1, if any.
    pub waiting: Option<String>,
    pub loop_age_s: Option<f64>,
    pub generation: Option<u64>,
    pub hosts_live: Option<u64>,
    /// Unix seconds; 0 = no committed cut yet.
    pub last_cut_s: Option<u64>,
    pub cuts_committed: Option<u64>,
    pub cuts_aborted: Option<u64>,
    pub successions_open: Option<u64>,
    pub successions_declared: Option<u64>,
    pub revision: Option<String>,
    pub region_blocks: Option<String>,
}

pub fn parse(text: &str) -> PodMetrics {
    let mut m = PodMetrics::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((series, value)) = split_sample(line) else {
            continue;
        };
        let (name, labels) = match series.split_once('{') {
            Some((n, rest)) => (n, rest.trim_end_matches('}')),
            None => (series, ""),
        };
        let int = || {
            value
                .parse::<f64>()
                .ok()
                .filter(|v| *v >= 0.0)
                .map(|v| v as u64)
        };
        match name {
            "mmo_registered" => m.registered = int().map(|v| v == 1),
            "mmo_waiting" => {
                if int() == Some(1) {
                    m.waiting = label(labels, "why");
                }
            }
            "mmo_loop_age_seconds" => m.loop_age_s = value.parse().ok(),
            "mmo_world_generation" => m.generation = int(),
            "mmo_hosts_live" => m.hosts_live = int(),
            "mmo_cut_last_committed_timestamp_seconds" => m.last_cut_s = int(),
            "mmo_cuts_decided_total" => match label(labels, "outcome").as_deref() {
                Some("committed") => m.cuts_committed = int(),
                Some("aborted") => m.cuts_aborted = int(),
                _ => {}
            },
            "mmo_successions_open" => m.successions_open = int(),
            "mmo_successions_declared_total" => m.successions_declared = int(),
            "mmo_build_info" => {
                m.revision = label(labels, "revision");
                m.region_blocks = label(labels, "region_blocks");
            }
            _ => {}
        }
    }
    m
}

/// `series value [timestamp]` → (series, value); the series may hold spaces inside quotes.
fn split_sample(line: &str) -> Option<(&str, &str)> {
    let end = match line.find('{') {
        Some(open) => open + line[open..].find('}')? + 1,
        None => line.find(char::is_whitespace)?,
    };
    let (series, rest) = line.split_at(end);
    Some((series, rest.split_whitespace().next()?))
}

/// The value of `key="…"` in a label set (no escaped quotes in our labels).
fn label(labels: &str, key: &str) -> Option<String> {
    labels.split(',').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}
