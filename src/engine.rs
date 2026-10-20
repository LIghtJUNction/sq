use crate::{
    config::{Config, Strategy},
    protocol::{Request, validate_response},
};
use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

const MAX_RESPONSE: usize = 8 * 1024 * 1024;

struct Backend {
    name: String,
    endpoint: String,
    model: Option<String>,
    client: reqwest::Client,
}

pub struct Engine {
    backends: Vec<Backend>,
    strategy: Strategy,
    retries: u32,
    cursor: AtomicUsize,
    slots: Arc<Semaphore>,
}

#[derive(Debug)]
struct Failure {
    message: String,
    retryable: bool,
    retry_after: Option<u64>,
}

impl Engine {
    pub fn new(config: &Config) -> Result<Self> {
        let mut backends = vec![];
        for p in config.validate()? {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Some(key) = std::env::var_os(&p.api_key_env) {
                let key = key
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("{} is not UTF-8", p.api_key_env))?;
                anyhow::ensure!(!key.trim().is_empty(), "{} is empty", p.api_key_env);
                let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| {
                        anyhow::anyhow!("{} is not a valid HTTP credential", p.api_key_env)
                    })?;
                value.set_sensitive(true);
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
            let client = reqwest::Client::builder()
                .default_headers(headers)
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(config.timeout_secs))
                .connect_timeout(std::time::Duration::from_secs(config.timeout_secs.min(10)))
                .user_agent(concat!("sq/", env!("CARGO_PKG_VERSION")))
                .build()
                .context("cannot create HTTP client")?;
            backends.push(Backend {
                name: p.name,
                endpoint: format!("{}/v1/systemone", p.base_url),
                model: p.model,
                client,
            });
        }
        Ok(Self {
            backends,
            strategy: config.strategy,
            retries: config.retries,
            cursor: AtomicUsize::new(0),
            slots: Arc::new(Semaphore::new(config.concurrency)),
        })
    }

    pub async fn evaluate(&self, request: Request) -> Result<Value> {
        request.validate()?;
        anyhow::ensure!(
            request.model.is_none() || self.backends.len() == 1,
            "request model overrides require a single-provider pool; configure each provider model for fallback/round-robin"
        );
        let _slot = self.slots.acquire().await?;
        let start = match self.strategy {
            Strategy::Fallback => 0,
            Strategy::RoundRobin => {
                self.cursor.fetch_add(1, Ordering::Relaxed) % self.backends.len()
            }
        };
        let mut failures = vec![];
        for offset in 0..self.backends.len() {
            let backend = &self.backends[(start + offset) % self.backends.len()];
            let mut body = request.clone();
            if body.model.is_none() {
                body.model = backend.model.clone();
            }
            for attempt in 0..=self.retries {
                match backend.send(&body).await {
                    Ok(mut response) => {
                        response
                            .as_object_mut()
                            .context("response must be an object")?
                            .insert("_sq".into(), serde_json::json!({"provider": backend.name}));
                        return Ok(response);
                    }
                    Err(e) => {
                        let retry = e.retryable;
                        failures.push(format!("{}: {}", backend.name, e.message));
                        if !retry {
                            bail!("{}", failures.join("; "));
                        }
                        if attempt < self.retries {
                            let millis = e
                                .retry_after
                                .map(|s| s.min(30) * 1000)
                                .unwrap_or(200 * (1u64 << attempt));
                            tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
                        }
                    }
                }
            }
        }
        bail!("all providers failed: {}", failures.join("; "))
    }
}

impl Backend {
    async fn send(&self, request: &Request) -> std::result::Result<Value, Failure> {
        let response = self
            .client
            .post(&self.endpoint)
            .json(request)
            .send()
            .await
            .map_err(|e| Failure {
                message: if e.is_timeout() {
                    "request timed out"
                } else if e.is_connect() {
                    "connection failed"
                } else {
                    "transport failed"
                }
                .into(),
                retryable: e.is_timeout() || e.is_connect(),
                retry_after: None,
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(Failure {
                message: format!("HTTP {}", status.as_u16()),
                retryable: status.as_u16() == 429 || status.is_server_error(),
                retry_after: response
                    .headers()
                    .get("retry-after")
                    .and_then(|s| s.to_str().ok())
                    .and_then(|s| s.parse().ok()),
            });
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| Failure {
                message: "response transfer failed".into(),
                retryable: true,
                retry_after: None,
            })?;
            if bytes.len() + chunk.len() > MAX_RESPONSE {
                return Err(Failure {
                    message: "response exceeds 8 MiB".into(),
                    retryable: false,
                    retry_after: None,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: Value = serde_json::from_slice(&bytes).map_err(|_| Failure {
            message: "invalid JSON response".into(),
            retryable: true,
            retry_after: None,
        })?;
        validate_response(request, &response).map_err(|e| Failure {
            message: format!("invalid System One response: {e}"),
            retryable: true,
            retry_after: None,
        })?;
        Ok(response)
    }
}
