// Copyright (c) 2026 Interchouette-ITC
// SPDX-License-Identifier: BUSL-1.1

//! Text embedding clients for RAG.

use crate::llm::ollama::embed_keep_alive_json;
use async_trait::async_trait;
use std::time::Duration;
use thiserror::Error;
use tracing::info;

/// Max wait for boot warm `/api/embed`. Past this, boot fails hard.
/// Keep in sync with chat warm in `llm::ollama` (cold load headroom).
const EMBED_WARM_TIMEOUT: Duration = Duration::from_mins(5);

/// Errors from embedding providers.
#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("embed request failed: {0}")]
    Request(String),
    #[error("embed parse: {0}")]
    Parse(String),
    #[error("embed unavailable: {0}")]
    Unavailable(String),
}

/// Client for text embeddings.
#[async_trait]
pub trait EmbedClient: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn embed(&self, model: &str, text: &str) -> Result<Vec<f32>, EmbedError>;
    /// Pin embed model in Ollama memory. Default: no-op.
    async fn warm_model(&self, _model: &str) -> Result<(), EmbedError> {
        Ok(())
    }
}

/// Ollama `/api/embed` client.
pub struct OllamaEmbedClient {
    base_url: String,
    http: reqwest::Client,
}

impl OllamaEmbedClient {
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl EmbedClient for OllamaEmbedClient {
    fn provider_id(&self) -> &'static str {
        "ollama"
    }

    async fn embed(&self, model: &str, text: &str) -> Result<Vec<f32>, EmbedError> {
        let url = format!("{}/api/embed", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "input": text,
            "keep_alive": embed_keep_alive_json(),
        });
        let res = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbedError::Request(e.to_string()))?;
        let status = res.status();
        let text_res = res
            .text()
            .await
            .map_err(|e| EmbedError::Request(e.to_string()))?;
        if !status.is_success() {
            return Err(EmbedError::Request(format!(
                "Ollama embed {status}: {text_res}"
            )));
        }
        let value: serde_json::Value =
            serde_json::from_str(&text_res).map_err(|e| EmbedError::Parse(e.to_string()))?;
        if let Some(err) = value.get("error") {
            return Err(EmbedError::Request(format!("Ollama error: {err}")));
        }
        let embeddings = value
            .get("embeddings")
            .and_then(|e| e.as_array())
            .ok_or_else(|| EmbedError::Parse("missing embeddings".into()))?;
        let first = embeddings
            .first()
            .ok_or_else(|| EmbedError::Parse("empty embeddings".into()))?;
        let arr = first
            .as_array()
            .ok_or_else(|| EmbedError::Parse("embedding not an array".into()))?;
        let vec: Vec<f32> = arr
            .iter()
            .filter_map(|v| {
                v.as_f64().map(|f| {
                    #[expect(clippy::cast_possible_truncation)]
                    {
                        f as f32
                    }
                })
            })
            .collect();
        if vec.len() != arr.len() {
            return Err(EmbedError::Parse("embedding contained non-numbers".into()));
        }
        Ok(vec)
    }

    async fn warm_model(&self, model: &str) -> Result<(), EmbedError> {
        let url = format!("{}/api/embed", self.base_url);
        let body = serde_json::json!({
            "model": model,
            "input": "warm",
            "keep_alive": embed_keep_alive_json(),
        });
        let send = self.http.post(&url).json(&body).send();
        let res = tokio::time::timeout(EMBED_WARM_TIMEOUT, send)
            .await
            .map_err(|_| {
                EmbedError::Request(format!(
                    "warm {model}: timed out after {}s",
                    EMBED_WARM_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| EmbedError::Request(format!("warm {model}: {e}")))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            let snippet: String = text.chars().take(400).collect();
            return Err(EmbedError::Request(format!(
                "warm {model}: HTTP {status}: {snippet}"
            )));
        }
        info!(model = %model, keep_alive = %embed_keep_alive_json(), "ollama: embed model warm");
        Ok(())
    }
}

/// Deterministic bag-of-bytes embedding for tests (no live Ollama).
pub struct MockEmbedClient;

#[async_trait]
impl EmbedClient for MockEmbedClient {
    fn provider_id(&self) -> &'static str {
        "mock"
    }

    async fn embed(&self, _model: &str, text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(mock_embed(text))
    }
}

/// Builds a fixed-size normalized vector from text bytes (stable across runs).
#[must_use]
pub fn mock_embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; 32];
    let lower = text.to_ascii_lowercase();
    for (i, b) in lower.bytes().enumerate() {
        v[i % 32] += f32::from(b) / 255.0;
    }
    // Boost shared tokens lightly so similar subjects cluster.
    for token in lower.split(|c: char| !c.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let mut h: u32 = 0;
        for b in token.bytes() {
            h = h.wrapping_mul(31).wrapping_add(u32::from(b));
        }
        let idx = (h as usize) % 32;
        v[idx] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Resolves an embed client from env (Ollama when reachable config present).
#[must_use]
pub fn build_embed_client() -> std::sync::Arc<dyn EmbedClient> {
    let base = std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".into());
    std::sync::Arc::new(OllamaEmbedClient::new(base))
}

/// Default embed model id (Ollama).
#[must_use]
pub fn default_embed_model() -> String {
    std::env::var("ITCY_EMBED_MODEL").unwrap_or_else(|_| "nomic-embed-text".into())
}
