//! The few Kubernetes API calls the panel makes, over `reqwest` (rustls, ring).
//!
//! Two ways in: inside the cluster with the pod's ServiceAccount (token and CA from
//! the projected volume; the token is re-read on every request, it rotates), or
//! through `kubectl proxy` (`PANEL_KUBE_API=http://127.0.0.1:8001`), which adds the
//! caller's own credentials — how `just panel` runs it on a workstation.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use serde_json::Value;

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
/// Every call but a followed log gives up after this.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum KubeError {
    NotFound,
    Forbidden(String),
    Status(u16, String),
    Transport(String),
}

impl std::fmt::Display for KubeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KubeError::NotFound => write!(f, "not found"),
            KubeError::Forbidden(m) => write!(f, "forbidden: {m}"),
            KubeError::Status(code, m) => write!(f, "HTTP {code}: {m}"),
            KubeError::Transport(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for KubeError {}

#[derive(Clone)]
pub struct Kube {
    http: reqwest::Client,
    base: String,
    token_file: Option<PathBuf>,
}

impl Kube {
    /// `PANEL_KUBE_API` set: that base URL, no credentials (kubectl proxy).
    /// Unset: the in-cluster API server with the pod's ServiceAccount.
    pub fn from_env() -> Result<Kube, String> {
        match std::env::var("PANEL_KUBE_API") {
            Ok(base) if !base.is_empty() => Ok(Kube::plain(&base)),
            _ => Kube::in_cluster(),
        }
    }

    /// An API server (or a proxy to one) that needs no credentials from us.
    pub fn plain(base: &str) -> Kube {
        Kube {
            http: reqwest::Client::builder().build().expect("reqwest client"),
            base: base.trim_end_matches('/').to_string(),
            token_file: None,
        }
    }

    fn in_cluster() -> Result<Kube, String> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|_| "neither PANEL_KUBE_API nor KUBERNETES_SERVICE_HOST is set".to_string())?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let ca = std::fs::read(format!("{SA_DIR}/ca.crt"))
            .map_err(|e| format!("{SA_DIR}/ca.crt: {e}"))?;
        let ca = reqwest::Certificate::from_pem(&ca).map_err(|e| format!("ca.crt: {e}"))?;
        let http = reqwest::Client::builder()
            .add_root_certificate(ca)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        // An IPv6 service host needs brackets in a URL.
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        Ok(Kube {
            http,
            base: format!("https://{host}:{port}"),
            token_file: Some(PathBuf::from(format!("{SA_DIR}/token"))),
        })
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, KubeError> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(file) = &self.token_file {
            let token = std::fs::read_to_string(file)
                .map_err(|e| KubeError::Transport(format!("token: {e}")))?;
            req = req.bearer_auth(token.trim());
        }
        Ok(req)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Response, KubeError> {
        req.send()
            .await
            .map_err(|e| KubeError::Transport(format!("API server: {e}")))
    }

    async fn json(resp: Response) -> Result<Value, KubeError> {
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| KubeError::Transport(e.to_string()))?;
        match status {
            200..=299 => serde_json::from_str(&body)
                .map_err(|e| KubeError::Transport(format!("bad JSON: {e}"))),
            404 => Err(KubeError::NotFound),
            401 | 403 => Err(KubeError::Forbidden(status_message(&body))),
            _ => Err(KubeError::Status(status, status_message(&body))),
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value, KubeError> {
        let req = self.request(Method::GET, path)?.timeout(REQUEST_TIMEOUT);
        Kube::json(self.send(req).await?).await
    }

    /// Status and body, whatever the status (a pod's `/readyz` answers 503 with a reason).
    pub async fn get_text(&self, path: &str) -> Result<(u16, String), KubeError> {
        let req = self.request(Method::GET, path)?.timeout(REQUEST_TIMEOUT);
        let resp = self.send(req).await?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| KubeError::Transport(e.to_string()))?;
        Ok((status, body))
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, KubeError> {
        let req = self
            .request(Method::POST, path)?
            .timeout(REQUEST_TIMEOUT)
            .json(body);
        Kube::json(self.send(req).await?).await
    }

    /// A JSON merge patch.
    pub async fn patch(&self, path: &str, body: &Value) -> Result<Value, KubeError> {
        let req = self
            .request(Method::PATCH, path)?
            .timeout(REQUEST_TIMEOUT)
            .header("content-type", "application/merge-patch+json")
            .body(body.to_string());
        Kube::json(self.send(req).await?).await
    }

    /// Sets a Deployment's or StatefulSet's replicas through its `scale` subresource.
    pub async fn scale(
        &self,
        ns: &str,
        kind: Scalable,
        name: &str,
        replicas: u32,
    ) -> Result<(), KubeError> {
        let path = format!(
            "/apis/apps/v1/namespaces/{ns}/{}/{name}/scale",
            kind.plural()
        );
        self.patch(
            &path,
            &serde_json::json!({ "spec": { "replicas": replicas } }),
        )
        .await
        .map(|_| ())
    }

    /// A pod's log as a stream (`follow`): no overall timeout, it ends with the pod.
    pub async fn follow_log(&self, ns: &str, pod: &str, tail: u32) -> Result<Response, KubeError> {
        let path = format!("/api/v1/namespaces/{ns}/pods/{pod}/log?follow=true&tailLines={tail}");
        let resp = self.send(self.request(Method::GET, &path)?).await?;
        match resp.status().as_u16() {
            200 => Ok(resp),
            404 => Err(KubeError::NotFound),
            s => Err(KubeError::Status(
                s,
                status_message(&resp.text().await.unwrap_or_default()),
            )),
        }
    }

    /// The last `tail` lines of a pod's log.
    pub async fn log_tail(&self, ns: &str, pod: &str, tail: u32) -> Result<String, KubeError> {
        let path = format!("/api/v1/namespaces/{ns}/pods/{pod}/log?tailLines={tail}");
        match self.get_text(&path).await? {
            (200, body) => Ok(body),
            (404, _) => Err(KubeError::NotFound),
            (s, body) => Err(KubeError::Status(s, status_message(&body))),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Scalable {
    Deployment,
    StatefulSet,
}

impl Scalable {
    fn plural(self) -> &'static str {
        match self {
            Scalable::Deployment => "deployments",
            Scalable::StatefulSet => "statefulsets",
        }
    }
}

/// The `message` of a Kubernetes `Status` body, else the body itself (shortened).
fn status_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect())
}
