//! The few Kubernetes API calls the world's own tools make, over `reqwest` (rustls, ring).
//! Copied from the panel's client (`crates/panel/src/kube.rs`), with `list` (by label), `put`,
//! `delete` and [`KubeError::Conflict`] added for the fleet reconciler (`VOXEL_ROLE=fleet`).
//!
//! Two ways in: inside the cluster with the pod's ServiceAccount (token and CA from the
//! projected volume; the token is re-read on every request, it rotates), or through
//! `kubectl proxy` (the variable the caller names, e.g. `VOXEL_FLEET_KUBE_API=http://127.0.0.1:8001`),
//! which adds the caller's own credentials — how a recipe runs a tool from a workstation.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use serde_json::Value;

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";
/// Every call gives up after this.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KubeError {
    /// 404.
    NotFound,
    /// 409: the object exists already (a create), or it changed since it was read (a replace
    /// carrying its `resourceVersion`).
    Conflict,
    /// 401 / 403, with the API server's message.
    Forbidden(String),
    /// Any other status that is not a success, with the API server's message.
    Status(u16, String),
    /// No answer: the connection, a timeout, an unreadable body.
    Transport(String),
}

impl std::fmt::Display for KubeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KubeError::NotFound => write!(f, "not found"),
            KubeError::Conflict => write!(f, "conflict"),
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
    /// `api_env` (the name of an environment variable) set and not empty: that base URL, no
    /// credentials (kubectl proxy). Unset: the in-cluster API server with the pod's ServiceAccount.
    pub fn from_env(api_env: &str) -> Result<Kube, String> {
        match std::env::var(api_env) {
            Ok(base) if !base.trim().is_empty() => Ok(Kube::plain(base.trim())),
            _ => Kube::in_cluster(api_env),
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

    fn in_cluster(api_env: &str) -> Result<Kube, String> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .map_err(|_| format!("neither {api_env} nor KUBERNETES_SERVICE_HOST is set"))?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let ca = std::fs::read(format!("{SA_DIR}/ca.crt")).map_err(|e| format!("{SA_DIR}/ca.crt: {e}"))?;
        let ca = reqwest::Certificate::from_pem(&ca).map_err(|e| format!("ca.crt: {e}"))?;
        let http = reqwest::Client::builder()
            .add_root_certificate(ca)
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        // An IPv6 service host needs brackets in a URL.
        let host = if host.contains(':') { format!("[{host}]") } else { host };
        Ok(Kube {
            http,
            base: format!("https://{host}:{port}"),
            token_file: Some(PathBuf::from(format!("{SA_DIR}/token"))),
        })
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, KubeError> {
        let mut req = self.http.request(method, format!("{}{path}", self.base)).timeout(REQUEST_TIMEOUT);
        if let Some(file) = &self.token_file {
            let token = std::fs::read_to_string(file).map_err(|e| KubeError::Transport(format!("token: {e}")))?;
            req = req.bearer_auth(token.trim());
        }
        Ok(req)
    }

    async fn send(&self, req: RequestBuilder) -> Result<Response, KubeError> {
        req.send().await.map_err(|e| KubeError::Transport(format!("API server: {e}")))
    }

    /// The status and body of an answer: a success's body as JSON, anything else as its error.
    async fn answer(resp: Response) -> Result<(u16, String), KubeError> {
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| KubeError::Transport(e.to_string()))?;
        match status {
            200..=299 => Ok((status, body)),
            404 => Err(KubeError::NotFound),
            409 => Err(KubeError::Conflict),
            401 | 403 => Err(KubeError::Forbidden(status_message(&body))),
            _ => Err(KubeError::Status(status, status_message(&body))),
        }
    }

    async fn json(resp: Response) -> Result<Value, KubeError> {
        let (_, body) = Kube::answer(resp).await?;
        serde_json::from_str(&body).map_err(|e| KubeError::Transport(format!("bad JSON: {e}")))
    }

    pub async fn get(&self, path: &str) -> Result<Value, KubeError> {
        let req = self.request(Method::GET, path)?;
        Kube::json(self.send(req).await?).await
    }

    /// The `items` of the collection at `path` whose labels match `label_selector` (e.g.
    /// `errantry/fleet=errantry`; encoded here). One page: no `limit`, no `continue`.
    pub async fn list(&self, path: &str, label_selector: &str) -> Result<Vec<Value>, KubeError> {
        let sep = if path.contains('?') { '&' } else { '?' };
        let path = format!("{path}{sep}labelSelector={}", encode_query(label_selector));
        let list = self.get(&path).await?;
        match list.get("items") {
            Some(Value::Array(items)) => Ok(items.clone()),
            // An empty list may come without `items`.
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(_) => Err(KubeError::Transport(format!("{path}: `items` is not a list"))),
        }
    }

    /// Creates `body` in the collection at `path`. 409 (it exists) is [`KubeError::Conflict`].
    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, KubeError> {
        let req = self.request(Method::POST, path)?.json(body);
        Kube::json(self.send(req).await?).await
    }

    /// Replaces the object at `path` with `body`. With `metadata.resourceVersion` in `body`, a
    /// change since that version is [`KubeError::Conflict`] and nothing is written.
    pub async fn put(&self, path: &str, body: &Value) -> Result<Value, KubeError> {
        let req = self.request(Method::PUT, path)?.json(body);
        Kube::json(self.send(req).await?).await
    }

    /// A JSON merge patch.
    pub async fn patch(&self, path: &str, body: &Value) -> Result<Value, KubeError> {
        let req = self
            .request(Method::PATCH, path)?
            .header("content-type", "application/merge-patch+json")
            .body(body.to_string());
        Kube::json(self.send(req).await?).await
    }

    /// Deletes the object at `path` (a query such as `?propagationPolicy=Background` may follow).
    /// An object already gone (404) is `Ok`.
    pub async fn delete(&self, path: &str) -> Result<(), KubeError> {
        let req = self.request(Method::DELETE, path)?;
        match Kube::answer(self.send(req).await?).await {
            Ok(_) | Err(KubeError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// `value` percent-encoded for a URL query (RFC 3986 unreserved characters kept).
pub fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `message` of a Kubernetes `Status` body, else the body itself (shortened).
fn status_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect())
}
