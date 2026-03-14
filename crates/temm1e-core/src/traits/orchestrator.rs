use crate::types::error::Temm1eError;
use async_trait::async_trait;

/// Orchestrator trait — container/VM lifecycle management (stub for v0.1)
#[async_trait]
pub trait Orchestrator: Send + Sync {
    async fn provision(&self, spec: AgentSpec) -> Result<AgentInstance, Temm1eError>;
    async fn scale(&self, instance: &AgentInstance, replicas: u32) -> Result<(), Temm1eError>;
    async fn destroy(&self, instance: &AgentInstance) -> Result<(), Temm1eError>;
    async fn health(&self, instance: &AgentInstance) -> Result<bool, Temm1eError>;
    fn backend_name(&self) -> &str;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentSpec {
    pub name: String,
    pub image: String,
    pub env: std::collections::HashMap<String, String>,
    pub resources: ResourceLimits,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceLimits {
    pub memory_mb: u64,
    pub cpu_millicores: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentInstance {
    pub id: String,
    pub name: String,
    pub status: String,
    pub url: Option<String>,
}
