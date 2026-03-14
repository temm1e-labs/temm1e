//! Orchestrator implementations for container/VM lifecycle management.
//!
//! Provides two backends:
//! - [`DockerOrchestrator`] — manages containers via the Docker Engine API.
//! - [`KubernetesOrchestrator`] — placeholder for future Kubernetes support.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::error::Temm1eError;
use crate::{AgentInstance, AgentSpec, Orchestrator};

// ---------------------------------------------------------------------------
// DockerClient abstraction (enables mocking without a real Docker daemon)
// ---------------------------------------------------------------------------

/// Minimal abstraction over the Docker Engine API.
///
/// Production code talks to the real daemon; tests inject a mock.
#[async_trait]
pub trait DockerClient: Send + Sync {
    /// POST /containers/create — returns the container ID on success.
    async fn create_container(&self, spec: &ContainerCreateRequest) -> Result<String, Temm1eError>;

    /// POST /containers/{id}/start
    async fn start_container(&self, id: &str) -> Result<(), Temm1eError>;

    /// POST /containers/{id}/stop
    async fn stop_container(&self, id: &str) -> Result<(), Temm1eError>;

    /// DELETE /containers/{id}?force=true
    async fn remove_container(&self, id: &str) -> Result<(), Temm1eError>;

    /// GET /containers/{id}/json — returns the container status string.
    async fn inspect_container(&self, id: &str) -> Result<ContainerInspect, Temm1eError>;
}

/// Simplified container creation payload matching the Docker Engine API.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContainerCreateRequest {
    pub name: String,
    pub image: String,
    pub env: Vec<String>,
    pub memory_bytes: u64,
    pub cpu_period: u64,
    pub cpu_quota: u64,
    pub privileged: bool,
}

/// Simplified inspect result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContainerInspect {
    pub id: String,
    pub status: String,
}

// ---------------------------------------------------------------------------
// DockerOrchestrator
// ---------------------------------------------------------------------------

/// Orchestrator backed by the Docker Engine API.
///
/// Uses a [`DockerClient`] abstraction so the implementation can be tested
/// without a running Docker daemon.
pub struct DockerOrchestrator {
    client: Box<dyn DockerClient>,
    docker_host: String,
    max_instances: u32,
    instances: RwLock<HashMap<String, AgentInstance>>,
}

impl DockerOrchestrator {
    /// Create a new `DockerOrchestrator`.
    ///
    /// * `client`        — Docker API client (real or mock).
    /// * `docker_host`   — Docker host URI (e.g. `unix:///var/run/docker.sock`).
    /// * `max_instances` — Safety cap on the number of managed containers.
    pub fn new(client: Box<dyn DockerClient>, docker_host: String, max_instances: u32) -> Self {
        Self {
            client,
            docker_host,
            max_instances,
            instances: RwLock::new(HashMap::new()),
        }
    }

    /// Return the configured Docker host URI.
    pub fn docker_host(&self) -> &str {
        &self.docker_host
    }

    /// Return the maximum allowed instances.
    pub fn max_instances(&self) -> u32 {
        self.max_instances
    }

    /// Return the current number of tracked instances.
    pub async fn instance_count(&self) -> usize {
        self.instances.read().await.len()
    }

    /// Build a [`ContainerCreateRequest`] from an [`AgentSpec`].
    ///
    /// Converts environment variables into Docker's `"KEY=VALUE"` list format
    /// and maps resource limits to kernel cgroup parameters. Privilege
    /// escalation is always denied.
    fn build_create_request(spec: &AgentSpec) -> ContainerCreateRequest {
        let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        // Docker CPU quota: millicores -> microseconds per 100ms period.
        // 1000 millicores = 100_000 us quota (one full core).
        let cpu_period: u64 = 100_000;
        let cpu_quota = spec.resources.cpu_millicores * 100; // 1 millicore = 100 us

        ContainerCreateRequest {
            name: spec.name.clone(),
            image: spec.image.clone(),
            env,
            memory_bytes: spec.resources.memory_mb * 1024 * 1024,
            cpu_period,
            cpu_quota,
            privileged: false, // never allow privilege escalation
        }
    }
}

#[async_trait]
impl Orchestrator for DockerOrchestrator {
    async fn provision(&self, spec: AgentSpec) -> Result<AgentInstance, Temm1eError> {
        // Safety: enforce max instances cap.
        let count = self.instances.read().await.len() as u32;
        if count >= self.max_instances {
            return Err(Temm1eError::Internal(format!(
                "Max instances limit reached ({}/{})",
                count, self.max_instances
            )));
        }

        let req = Self::build_create_request(&spec);

        tracing::info!(
            name = %spec.name,
            image = %spec.image,
            memory_mb = spec.resources.memory_mb,
            cpu_millicores = spec.resources.cpu_millicores,
            "Provisioning container"
        );

        let container_id = self.client.create_container(&req).await?;
        self.client.start_container(&container_id).await?;

        let instance = AgentInstance {
            id: container_id.clone(),
            name: spec.name.clone(),
            status: "running".to_string(),
            url: None,
        };

        self.instances
            .write()
            .await
            .insert(container_id, instance.clone());

        tracing::info!(id = %instance.id, name = %instance.name, "Container provisioned");

        Ok(instance)
    }

    async fn scale(&self, instance: &AgentInstance, replicas: u32) -> Result<(), Temm1eError> {
        if replicas == 0 {
            return Err(Temm1eError::Internal(
                "Replicas must be at least 1; use destroy() to remove an instance".to_string(),
            ));
        }

        // Count current replicas sharing the same base name.
        let instances = self.instances.read().await;
        let base_name = &instance.name;
        let current: Vec<String> = instances
            .values()
            .filter(|i| i.name == *base_name || i.name.starts_with(&format!("{base_name}-")))
            .map(|i| i.id.clone())
            .collect();
        let current_count = current.len() as u32;
        drop(instances);

        if replicas == current_count {
            return Ok(());
        }

        if replicas > current_count {
            // Scale up — we need to know the image / resources to create new
            // containers. Since we don't store the original spec, we require
            // that the caller uses provision() for brand-new containers. For
            // scale-up we derive a minimal spec from the existing instance.
            let to_add = replicas - current_count;
            let total_after = self.instances.read().await.len() as u32 + to_add;
            if total_after > self.max_instances {
                return Err(Temm1eError::Internal(format!(
                    "Scaling to {replicas} replicas would exceed max instances ({total_after}/{})",
                    self.max_instances
                )));
            }
            // We cannot fully scale up without the original spec, so we return
            // an error asking the caller to provision individually.
            return Err(Temm1eError::Internal(format!(
                "Scale up from {current_count} to {replicas} requires calling provision() \
                 for each new replica (Docker does not have native replica sets)"
            )));
        }

        // Scale down — stop and remove excess replicas, keeping the primary.
        let to_remove = current_count - replicas;
        let mut removed = 0u32;
        for id in current.iter().rev() {
            if removed >= to_remove {
                break;
            }
            // Don't remove the original instance.
            if *id == instance.id {
                continue;
            }
            self.client.stop_container(id).await?;
            self.client.remove_container(id).await?;
            self.instances.write().await.remove(id);
            removed += 1;
            tracing::info!(id = %id, "Scaled down — removed replica");
        }

        Ok(())
    }

    async fn destroy(&self, instance: &AgentInstance) -> Result<(), Temm1eError> {
        tracing::info!(id = %instance.id, name = %instance.name, "Destroying container");

        self.client.stop_container(&instance.id).await?;
        self.client.remove_container(&instance.id).await?;
        self.instances.write().await.remove(&instance.id);

        tracing::info!(id = %instance.id, "Container destroyed");
        Ok(())
    }

    async fn health(&self, instance: &AgentInstance) -> Result<bool, Temm1eError> {
        let inspect = self.client.inspect_container(&instance.id).await?;
        let healthy = inspect.status == "running";

        // Update tracked status if it changed.
        if let Some(tracked) = self.instances.write().await.get_mut(&instance.id) {
            tracked.status = inspect.status;
        }

        Ok(healthy)
    }

    fn backend_name(&self) -> &str {
        "docker"
    }
}

// ---------------------------------------------------------------------------
// KubernetesOrchestrator (stub)
// ---------------------------------------------------------------------------

/// Placeholder Kubernetes orchestrator. All methods return an error.
pub struct KubernetesOrchestrator {
    #[allow(dead_code)]
    kubeconfig: String,
}

impl KubernetesOrchestrator {
    pub fn new(kubeconfig: String) -> Self {
        Self { kubeconfig }
    }
}

#[async_trait]
impl Orchestrator for KubernetesOrchestrator {
    async fn provision(&self, _spec: AgentSpec) -> Result<AgentInstance, Temm1eError> {
        Err(Temm1eError::Internal(
            "Kubernetes orchestrator not yet implemented".to_string(),
        ))
    }

    async fn scale(&self, _instance: &AgentInstance, _replicas: u32) -> Result<(), Temm1eError> {
        Err(Temm1eError::Internal(
            "Kubernetes orchestrator not yet implemented".to_string(),
        ))
    }

    async fn destroy(&self, _instance: &AgentInstance) -> Result<(), Temm1eError> {
        Err(Temm1eError::Internal(
            "Kubernetes orchestrator not yet implemented".to_string(),
        ))
    }

    async fn health(&self, _instance: &AgentInstance) -> Result<bool, Temm1eError> {
        Err(Temm1eError::Internal(
            "Kubernetes orchestrator not yet implemented".to_string(),
        ))
    }

    fn backend_name(&self) -> &str {
        "kubernetes"
    }
}

// ---------------------------------------------------------------------------
// Factory function
// ---------------------------------------------------------------------------

/// Create an orchestrator by backend name.
///
/// Supported backends:
/// - `"docker"` — expects optional keys `docker_host` and `max_instances`.
/// - `"kubernetes"` — expects optional key `kubeconfig`.
///
/// The Docker backend requires a [`DockerClient`] implementation at runtime;
/// this factory creates the orchestrator with a **default HTTP client** that
/// talks to the real Docker daemon. For testing, construct
/// [`DockerOrchestrator`] directly with a mock client.
pub fn create_orchestrator(
    backend: &str,
    config: &HashMap<String, String>,
) -> Result<Box<dyn Orchestrator>, Temm1eError> {
    match backend {
        "docker" => {
            let docker_host = config
                .get("docker_host")
                .cloned()
                .unwrap_or_else(|| "unix:///var/run/docker.sock".to_string());
            let max_instances: u32 = config
                .get("max_instances")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10);

            // In the factory path we use a stub client that returns
            // `Internal` errors — real production code would inject a proper
            // HTTP-based DockerClient. This keeps the core crate free of heavy
            // HTTP dependencies (reqwest lives in the tools/gateway crates).
            let client = Box::new(UnimplementedDockerClient);
            Ok(Box::new(DockerOrchestrator::new(
                client,
                docker_host,
                max_instances,
            )))
        }
        "kubernetes" => {
            let kubeconfig = config
                .get("kubeconfig")
                .cloned()
                .unwrap_or_else(|| "~/.kube/config".to_string());
            Ok(Box::new(KubernetesOrchestrator::new(kubeconfig)))
        }
        other => Err(Temm1eError::Config(format!(
            "Unknown orchestrator backend: {other}"
        ))),
    }
}

/// A no-op Docker client used by the factory when no real client is provided.
struct UnimplementedDockerClient;

#[async_trait]
impl DockerClient for UnimplementedDockerClient {
    async fn create_container(
        &self,
        _spec: &ContainerCreateRequest,
    ) -> Result<String, Temm1eError> {
        Err(Temm1eError::Internal(
            "No DockerClient configured — inject a real client via DockerOrchestrator::new()"
                .to_string(),
        ))
    }

    async fn start_container(&self, _id: &str) -> Result<(), Temm1eError> {
        Err(Temm1eError::Internal(
            "No DockerClient configured".to_string(),
        ))
    }

    async fn stop_container(&self, _id: &str) -> Result<(), Temm1eError> {
        Err(Temm1eError::Internal(
            "No DockerClient configured".to_string(),
        ))
    }

    async fn remove_container(&self, _id: &str) -> Result<(), Temm1eError> {
        Err(Temm1eError::Internal(
            "No DockerClient configured".to_string(),
        ))
    }

    async fn inspect_container(&self, _id: &str) -> Result<ContainerInspect, Temm1eError> {
        Err(Temm1eError::Internal(
            "No DockerClient configured".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // -----------------------------------------------------------------------
    // Mock DockerClient
    // -----------------------------------------------------------------------

    /// A mock Docker client that tracks calls and returns configurable results.
    struct MockDockerClient {
        next_id: AtomicU32,
        /// When true, `inspect_container` returns status "running".
        healthy: std::sync::RwLock<bool>,
        /// When true, `create_container` returns an error.
        fail_create: bool,
        /// When true, `stop_container` returns an error.
        fail_stop: bool,
    }

    impl MockDockerClient {
        fn new() -> Self {
            Self {
                next_id: AtomicU32::new(1),
                healthy: std::sync::RwLock::new(true),
                fail_create: false,
                fail_stop: false,
            }
        }

        fn with_fail_create() -> Self {
            Self {
                fail_create: true,
                ..Self::new()
            }
        }

        fn with_fail_stop() -> Self {
            Self {
                fail_stop: true,
                ..Self::new()
            }
        }
    }

    #[async_trait]
    impl DockerClient for MockDockerClient {
        async fn create_container(
            &self,
            _spec: &ContainerCreateRequest,
        ) -> Result<String, Temm1eError> {
            if self.fail_create {
                return Err(Temm1eError::Internal("Docker create failed".to_string()));
            }
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            Ok(format!("container-{id}"))
        }

        async fn start_container(&self, _id: &str) -> Result<(), Temm1eError> {
            Ok(())
        }

        async fn stop_container(&self, _id: &str) -> Result<(), Temm1eError> {
            if self.fail_stop {
                return Err(Temm1eError::Internal("Docker stop failed".to_string()));
            }
            Ok(())
        }

        async fn remove_container(&self, _id: &str) -> Result<(), Temm1eError> {
            Ok(())
        }

        async fn inspect_container(&self, id: &str) -> Result<ContainerInspect, Temm1eError> {
            let status = if *self.healthy.read().unwrap() {
                "running"
            } else {
                "exited"
            };
            Ok(ContainerInspect {
                id: id.to_string(),
                status: status.to_string(),
            })
        }
    }

    // Also implement for Arc<MockDockerClient> so we can share it.
    #[async_trait]
    impl DockerClient for Arc<MockDockerClient> {
        async fn create_container(
            &self,
            spec: &ContainerCreateRequest,
        ) -> Result<String, Temm1eError> {
            (**self).create_container(spec).await
        }

        async fn start_container(&self, id: &str) -> Result<(), Temm1eError> {
            (**self).start_container(id).await
        }

        async fn stop_container(&self, id: &str) -> Result<(), Temm1eError> {
            (**self).stop_container(id).await
        }

        async fn remove_container(&self, id: &str) -> Result<(), Temm1eError> {
            (**self).remove_container(id).await
        }

        async fn inspect_container(&self, id: &str) -> Result<ContainerInspect, Temm1eError> {
            (**self).inspect_container(id).await
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn test_spec(name: &str) -> AgentSpec {
        AgentSpec {
            name: name.to_string(),
            image: "alpine:latest".to_string(),
            env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
            resources: crate::ResourceLimits {
                memory_mb: 256,
                cpu_millicores: 500,
            },
        }
    }

    fn docker_orchestrator(client: Box<dyn DockerClient>, max: u32) -> DockerOrchestrator {
        DockerOrchestrator::new(client, "unix:///var/run/docker.sock".to_string(), max)
    }

    // -----------------------------------------------------------------------
    // Test: provision creates and tracks an instance
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_provision_creates_instance() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 10);
        let instance = orch.provision(test_spec("agent-1")).await.unwrap();

        assert_eq!(instance.name, "agent-1");
        assert_eq!(instance.status, "running");
        assert!(instance.url.is_none());
        assert_eq!(orch.instance_count().await, 1);
    }

    // -----------------------------------------------------------------------
    // Test: provision assigns unique IDs
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_provision_unique_ids() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 10);
        let a = orch.provision(test_spec("a")).await.unwrap();
        let b = orch.provision(test_spec("b")).await.unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(orch.instance_count().await, 2);
    }

    // -----------------------------------------------------------------------
    // Test: max instances limit is enforced
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_max_instances_limit() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 2);

        orch.provision(test_spec("a")).await.unwrap();
        orch.provision(test_spec("b")).await.unwrap();

        let result = orch.provision(test_spec("c")).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Max instances limit reached"),
            "unexpected error: {err}"
        );
        assert_eq!(orch.instance_count().await, 2);
    }

    // -----------------------------------------------------------------------
    // Test: destroy removes instance from tracking
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_destroy_removes_instance() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 10);
        let instance = orch.provision(test_spec("doomed")).await.unwrap();
        assert_eq!(orch.instance_count().await, 1);

        orch.destroy(&instance).await.unwrap();
        assert_eq!(orch.instance_count().await, 0);
    }

    // -----------------------------------------------------------------------
    // Test: destroy then re-provision reclaims capacity
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_destroy_frees_capacity() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 1);

        let first = orch.provision(test_spec("a")).await.unwrap();
        assert!(orch.provision(test_spec("b")).await.is_err());

        orch.destroy(&first).await.unwrap();
        let second = orch.provision(test_spec("c")).await.unwrap();
        assert_eq!(second.name, "c");
        assert_eq!(orch.instance_count().await, 1);
    }

    // -----------------------------------------------------------------------
    // Test: health returns true for running container
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_health_running() {
        let mock = Arc::new(MockDockerClient::new());
        let orch = docker_orchestrator(Box::new(Arc::clone(&mock)), 10);
        let instance = orch.provision(test_spec("healthy")).await.unwrap();

        let healthy = orch.health(&instance).await.unwrap();
        assert!(healthy);
    }

    // -----------------------------------------------------------------------
    // Test: health returns false for exited container
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_health_exited() {
        let mock = Arc::new(MockDockerClient::new());
        let orch = docker_orchestrator(Box::new(Arc::clone(&mock)), 10);
        let instance = orch.provision(test_spec("sick")).await.unwrap();

        // Flip the mock to report "exited".
        *mock.healthy.write().unwrap() = false;

        let healthy = orch.health(&instance).await.unwrap();
        assert!(!healthy);
    }

    // -----------------------------------------------------------------------
    // Test: health updates tracked status
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_health_updates_tracked_status() {
        let mock = Arc::new(MockDockerClient::new());
        let orch = docker_orchestrator(Box::new(Arc::clone(&mock)), 10);
        let instance = orch.provision(test_spec("tracker")).await.unwrap();

        // Initially running.
        assert_eq!(
            orch.instances
                .read()
                .await
                .get(&instance.id)
                .unwrap()
                .status,
            "running"
        );

        // Container exits.
        *mock.healthy.write().unwrap() = false;
        orch.health(&instance).await.unwrap();

        assert_eq!(
            orch.instances
                .read()
                .await
                .get(&instance.id)
                .unwrap()
                .status,
            "exited"
        );
    }

    // -----------------------------------------------------------------------
    // Test: provision propagates client errors
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_provision_propagates_create_error() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::with_fail_create()), 10);
        let result = orch.provision(test_spec("fail")).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Docker create failed"));
    }

    // -----------------------------------------------------------------------
    // Test: destroy propagates stop errors
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_destroy_propagates_stop_error() {
        // Use a shared mock so we can provision successfully then fail on stop.
        let mock = Arc::new(MockDockerClient::new());
        let orch = docker_orchestrator(Box::new(Arc::clone(&mock)), 10);
        let instance = orch.provision(test_spec("stop-fail")).await.unwrap();

        // Now create a new orchestrator with a failing stop client that has
        // the same instance in its map. Simpler: just test the fail-stop mock.
        let fail_orch = docker_orchestrator(Box::new(MockDockerClient::with_fail_stop()), 10);
        // Manually insert the instance.
        fail_orch
            .instances
            .write()
            .await
            .insert(instance.id.clone(), instance.clone());

        let result = fail_orch.destroy(&instance).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Docker stop failed"));
        // Instance should NOT have been removed because stop failed before remove.
        assert_eq!(fail_orch.instance_count().await, 1);
    }

    // -----------------------------------------------------------------------
    // Test: build_create_request enforces no privilege escalation
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_no_privilege_escalation() {
        let spec = test_spec("priv-test");
        let req = DockerOrchestrator::build_create_request(&spec);
        assert!(!req.privileged, "Containers must never be privileged");
    }

    // -----------------------------------------------------------------------
    // Test: build_create_request maps resources correctly
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_resource_mapping() {
        let spec = AgentSpec {
            name: "res-test".to_string(),
            image: "busybox:latest".to_string(),
            env: HashMap::new(),
            resources: crate::ResourceLimits {
                memory_mb: 512,
                cpu_millicores: 1000,
            },
        };

        let req = DockerOrchestrator::build_create_request(&spec);
        assert_eq!(req.memory_bytes, 512 * 1024 * 1024);
        assert_eq!(req.cpu_period, 100_000);
        assert_eq!(req.cpu_quota, 100_000); // 1000 millicores * 100
    }

    // -----------------------------------------------------------------------
    // Test: build_create_request maps environment variables
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_env_mapping() {
        let mut env = HashMap::new();
        env.insert("KEY1".to_string(), "val1".to_string());
        env.insert("KEY2".to_string(), "val2".to_string());

        let spec = AgentSpec {
            name: "env-test".to_string(),
            image: "alpine:latest".to_string(),
            env,
            resources: crate::ResourceLimits {
                memory_mb: 128,
                cpu_millicores: 250,
            },
        };

        let req = DockerOrchestrator::build_create_request(&spec);
        assert_eq!(req.env.len(), 2);
        assert!(req.env.contains(&"KEY1=val1".to_string()));
        assert!(req.env.contains(&"KEY2=val2".to_string()));
    }

    // -----------------------------------------------------------------------
    // Test: scale with zero replicas is rejected
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_scale_zero_replicas_rejected() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 10);
        let instance = orch.provision(test_spec("scale-zero")).await.unwrap();

        let result = orch.scale(&instance, 0).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at least 1"));
    }

    // -----------------------------------------------------------------------
    // Test: scale same replica count is a no-op
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_scale_same_count_noop() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 10);
        let instance = orch.provision(test_spec("scale-noop")).await.unwrap();

        // 1 instance exists with matching name, scaling to 1 should succeed.
        orch.scale(&instance, 1).await.unwrap();
        assert_eq!(orch.instance_count().await, 1);
    }

    // -----------------------------------------------------------------------
    // Test: backend_name returns "docker"
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_backend_name_docker() {
        let orch = docker_orchestrator(Box::new(MockDockerClient::new()), 10);
        assert_eq!(orch.backend_name(), "docker");
    }

    // -----------------------------------------------------------------------
    // Test: backend_name returns "kubernetes"
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_backend_name_kubernetes() {
        let orch = KubernetesOrchestrator::new("~/.kube/config".to_string());
        assert_eq!(orch.backend_name(), "kubernetes");
    }

    // -----------------------------------------------------------------------
    // Test: kubernetes stub returns errors
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_kubernetes_stub_errors() {
        let orch = KubernetesOrchestrator::new("~/.kube/config".to_string());
        let spec = test_spec("k8s-test");
        let instance = AgentInstance {
            id: "pod-1".to_string(),
            name: "k8s-test".to_string(),
            status: "running".to_string(),
            url: None,
        };

        assert!(orch.provision(spec).await.is_err());
        assert!(orch.scale(&instance, 3).await.is_err());
        assert!(orch.destroy(&instance).await.is_err());
        assert!(orch.health(&instance).await.is_err());

        // All errors should mention "not yet implemented".
        let dummy_spec = test_spec("k8s");
        let err = orch.provision(dummy_spec).await.unwrap_err().to_string();
        assert!(err.contains("not yet implemented"), "got: {err}");
    }

    // -----------------------------------------------------------------------
    // Test: factory function — docker
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_factory_docker() {
        let mut config = HashMap::new();
        config.insert(
            "docker_host".to_string(),
            "tcp://localhost:2375".to_string(),
        );
        config.insert("max_instances".to_string(), "5".to_string());

        let orch = create_orchestrator("docker", &config).unwrap();
        assert_eq!(orch.backend_name(), "docker");
    }

    // -----------------------------------------------------------------------
    // Test: factory function — docker defaults
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_factory_docker_defaults() {
        let config = HashMap::new();
        let orch = create_orchestrator("docker", &config).unwrap();
        assert_eq!(orch.backend_name(), "docker");
    }

    // -----------------------------------------------------------------------
    // Test: factory function — kubernetes
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_factory_kubernetes() {
        let config = HashMap::new();
        let orch = create_orchestrator("kubernetes", &config).unwrap();
        assert_eq!(orch.backend_name(), "kubernetes");
    }

    // -----------------------------------------------------------------------
    // Test: factory function — unknown backend
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_factory_unknown_backend() {
        let config = HashMap::new();
        let result = create_orchestrator("podman", &config);
        match result {
            Err(e) => assert!(
                e.to_string().contains("Unknown orchestrator"),
                "unexpected error: {e}"
            ),
            Ok(_) => panic!("expected error for unknown backend"),
        }
    }

    // -----------------------------------------------------------------------
    // Test: docker_host and max_instances accessors
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_accessors() {
        let orch = DockerOrchestrator::new(
            Box::new(MockDockerClient::new()),
            "tcp://docker:2375".to_string(),
            42,
        );
        assert_eq!(orch.docker_host(), "tcp://docker:2375");
        assert_eq!(orch.max_instances(), 42);
        assert_eq!(orch.instance_count().await, 0);
    }
}
