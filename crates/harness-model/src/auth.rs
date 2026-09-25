//! HTTP inference authentication only; implementors receive no execution handle.
use crate::ModelError;
use async_trait::async_trait;
#[async_trait]
pub trait InferenceAuth: Send + Sync {
    async fn headers(&self, body: &serde_json::Value) -> Result<http::HeaderMap, ModelError>;
}
