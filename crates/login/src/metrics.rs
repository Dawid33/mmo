//! Prometheus metrics of the login service, served at `GET /metrics`.
//!
//! One [`Metrics`] per [`crate::AppState`] (its own registry, so tests never share counters).
//! Every event has one method; no caller builds a label by hand. Labels come from fixed sets
//! only: the matched route template (or `other`), a numeric status, an outcome, a world id of the
//! registry. Never an email, an account id, a token or a raw path.

use std::sync::Arc;

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry, TextEncoder,
};

/// Request-duration buckets, seconds.
const DURATION_BUCKETS: [f64; 6] = [0.005, 0.025, 0.1, 0.5, 2.0, 10.0];

/// How an identity-provider login ended (`login_oidc_logins_total{outcome}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OidcOutcome {
    Created,
    Existing,
    Adopted,
    EmailTaken,
    Refused,
    ProviderUnavailable,
}

impl OidcOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Existing => "existing",
            Self::Adopted => "adopted",
            Self::EmailTaken => "email_taken",
            Self::Refused => "refused",
            Self::ProviderUnavailable => "provider_unavailable",
        }
    }
}

/// How a join ended (`login_joins_total{outcome}`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinOutcome {
    Ok,
    Refused,
    WorldUnavailable,
    /// 503 `world_starting` (Elastic 4): no host took logins and the world is starting one.
    WorldStarting,
}

impl JoinOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Refused => "refused",
            Self::WorldUnavailable => "world_unavailable",
            Self::WorldStarting => "world_starting",
        }
    }
}

struct Inner {
    registry: Registry,
    requests: IntCounterVec,
    duration: HistogramVec,
    oidc_logins: IntCounterVec,
    refreshes: IntCounterVec,
    joins: IntCounterVec,
    host_link_up: IntGaugeVec,
    pending_kicks: IntGauge,
    jwks_refetches: IntCounterVec,
}

/// The service's metrics. Cheap to clone.
#[derive(Clone)]
pub struct Metrics(Arc<Inner>);

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

fn outcome_counter(registry: &Registry, name: &str, help: &str) -> IntCounterVec {
    let c = IntCounterVec::new(Opts::new(name, help), &["outcome"]).expect("valid metric");
    registry.register(Box::new(c.clone())).expect("metric registered once");
    c
}

impl Metrics {
    pub fn new() -> Metrics {
        let registry = Registry::new();
        let requests = IntCounterVec::new(
            Opts::new("login_http_requests_total", "HTTP requests by matched route template and status."),
            &["route", "status"],
        )
        .expect("valid metric");
        let duration = HistogramVec::new(
            HistogramOpts::new("login_http_request_duration_seconds", "HTTP request duration by matched route template.")
                .buckets(DURATION_BUCKETS.to_vec()),
            &["route"],
        )
        .expect("valid metric");
        let host_link_up = IntGaugeVec::new(
            Opts::new("login_host_link_up", "1 while the service link to a world's host is up."),
            &["world"],
        )
        .expect("valid metric");
        let pending_kicks = IntGauge::new("login_pending_kicks", "Kicks queued and not yet acknowledged by a host.")
            .expect("valid metric");
        let build_info = IntGaugeVec::new(Opts::new("login_build_info", "Build of the running service (always 1)."), &["version"])
            .expect("valid metric");
        let oidc_logins = outcome_counter(&registry, "login_oidc_logins_total", "Identity-provider logins by outcome.");
        let refreshes = outcome_counter(&registry, "login_refresh_total", "Refresh-token rotations by outcome.");
        let joins = outcome_counter(&registry, "login_joins_total", "Joins by outcome.");
        let jwks_refetches =
            outcome_counter(&registry, "login_jwks_refetch_total", "Refetches of the identity provider's keys by outcome.");
        for c in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(duration.clone()),
            Box::new(host_link_up.clone()),
            Box::new(pending_kicks.clone()),
            Box::new(build_info.clone()),
        ] {
            registry.register(c).expect("metric registered once");
        }
        build_info.with_label_values(&[env!("CARGO_PKG_VERSION")]).set(1);
        Metrics(Arc::new(Inner { registry, requests, duration, oidc_logins, refreshes, joins, host_link_up, pending_kicks, jwks_refetches }))
    }

    /// The text exposition format.
    pub fn render(&self) -> String {
        let mut out = Vec::new();
        if let Err(e) = TextEncoder::new().encode(&self.0.registry.gather(), &mut out) {
            log::error!("[login] encoding metrics: {e}");
        }
        String::from_utf8(out).unwrap_or_default()
    }

    /// One finished HTTP request. `route` is a matched route template or `other`.
    pub fn http_request(&self, route: &str, status: u16, seconds: f64) {
        self.0.requests.with_label_values(&[route, &status.to_string()]).inc();
        self.0.duration.with_label_values(&[route]).observe(seconds);
    }

    pub fn oidc_login(&self, outcome: OidcOutcome) {
        self.0.oidc_logins.with_label_values(&[outcome.label()]).inc();
    }

    pub fn refresh(&self, ok: bool) {
        self.0.refreshes.with_label_values(&[if ok { "ok" } else { "refused" }]).inc();
    }

    pub fn join(&self, outcome: JoinOutcome) {
        self.0.joins.with_label_values(&[outcome.label()]).inc();
    }

    pub fn host_link(&self, world: &str, up: bool) {
        self.0.host_link_up.with_label_values(&[world]).set(i64::from(up));
    }

    pub fn pending_kicks(&self, n: u64) {
        self.0.pending_kicks.set(i64::try_from(n).unwrap_or(i64::MAX));
    }

    pub fn jwks_refetch(&self, ok: bool) {
        self.0.jwks_refetches.with_label_values(&[if ok { "ok" } else { "failed" }]).inc();
    }
}
