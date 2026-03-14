use crate::types::error::Temm1eError;
use async_trait::async_trait;

/// Peripheral trait — hardware integration (stub for v0.1)
#[async_trait]
pub trait Peripheral: Send + Sync {
    fn name(&self) -> &str;
    async fn read(&self) -> Result<serde_json::Value, Temm1eError>;
    async fn write(&self, data: serde_json::Value) -> Result<(), Temm1eError>;
}
