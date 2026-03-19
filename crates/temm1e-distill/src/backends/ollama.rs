//! Ollama backend — model management for local serving.
//!
//! Ollama is inference-only. Fine-tuning happens externally (Unsloth/MLX).
//! This module handles: health check, model listing, model creation from GGUF,
//! model deletion, and embedding generation.

use serde::Deserialize;
use tracing;

const OLLAMA_BASE: &str = "http://localhost:11434";

/// Check if Ollama is running and accessible.
pub async fn is_available() -> bool {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    client
        .get(OLLAMA_BASE)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
pub struct OllamaModelList {
    pub models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    pub size: Option<u64>,
    pub details: Option<OllamaModelDetails>,
}

#[derive(Debug, Deserialize)]
pub struct OllamaModelDetails {
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub quantization_level: Option<String>,
}

/// List all locally available models.
pub async fn list_models() -> Result<Vec<OllamaModel>, temm1e_core::types::error::Temm1eError> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/tags", OLLAMA_BASE))
        .send()
        .await
        .map_err(|e| temm1e_core::types::error::Temm1eError::Tool(format!("Ollama list: {}", e)))?;

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(temm1e_core::types::error::Temm1eError::Tool(format!(
            "Ollama list error: {}",
            err
        )));
    }

    let list: OllamaModelList = resp.json().await.map_err(|e| {
        temm1e_core::types::error::Temm1eError::Tool(format!("Ollama parse: {}", e))
    })?;

    Ok(list.models)
}

/// Create a model from a GGUF file.
pub async fn create_model(
    name: &str,
    gguf_path: &str,
    system_prompt: &str,
) -> Result<(), temm1e_core::types::error::Temm1eError> {
    let modelfile = format!(
        "FROM {}\nSYSTEM \"\"\"{}\"\"\"\nPARAMETER temperature 0.7\nPARAMETER num_ctx 4096",
        gguf_path, system_prompt
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600)) // Model creation can take minutes
        .build()
        .map_err(|e| temm1e_core::types::error::Temm1eError::Tool(format!("HTTP client: {}", e)))?;

    let body = serde_json::json!({
        "model": name,
        "modelfile": modelfile,
        "stream": false
    });

    let resp = client
        .post(format!("{}/api/create", OLLAMA_BASE))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            temm1e_core::types::error::Temm1eError::Tool(format!("Ollama create: {}", e))
        })?;

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(temm1e_core::types::error::Temm1eError::Tool(format!(
            "Ollama create error: {}",
            err
        )));
    }

    tracing::info!(name = name, "Eigen-Tune: Ollama model created");
    Ok(())
}

/// Delete a model.
pub async fn delete_model(name: &str) -> Result<(), temm1e_core::types::error::Temm1eError> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({"model": name});

    client
        .delete(format!("{}/api/delete", OLLAMA_BASE))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            temm1e_core::types::error::Temm1eError::Tool(format!("Ollama delete: {}", e))
        })?;

    tracing::info!(name = name, "Eigen-Tune: Ollama model deleted");
    Ok(())
}

#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f64>>,
}

/// Generate embedding for text using local Ollama embedding model.
pub async fn embed(
    text: &str,
    model: &str,
) -> Result<Vec<f64>, temm1e_core::types::error::Temm1eError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| temm1e_core::types::error::Temm1eError::Tool(format!("HTTP client: {}", e)))?;

    let body = serde_json::json!({
        "model": model,
        "input": text
    });

    let resp = client
        .post(format!("{}/api/embed", OLLAMA_BASE))
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            temm1e_core::types::error::Temm1eError::Tool(format!("Ollama embed: {}", e))
        })?;

    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(temm1e_core::types::error::Temm1eError::Tool(format!(
            "Ollama embed error: {}",
            err
        )));
    }

    let parsed: OllamaEmbedResponse = resp
        .json()
        .await
        .map_err(|e| temm1e_core::types::error::Temm1eError::Tool(format!("Parse embed: {}", e)))?;

    parsed
        .embeddings
        .into_iter()
        .next()
        .ok_or_else(|| temm1e_core::types::error::Temm1eError::Tool("Empty embedding".into()))
}

/// Ensure the embedding model is available locally, pull if needed.
pub async fn ensure_embedding_model(
    model: &str,
) -> Result<(), temm1e_core::types::error::Temm1eError> {
    let models = list_models().await?;
    let has_model = models.iter().any(|m| m.name.starts_with(model));

    if !has_model {
        tracing::info!(model = model, "Eigen-Tune: pulling embedding model...");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| {
                temm1e_core::types::error::Temm1eError::Tool(format!("HTTP client: {}", e))
            })?;

        let body = serde_json::json!({
            "name": model,
            "stream": false
        });

        let resp = client
            .post(format!("{}/api/pull", OLLAMA_BASE))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                temm1e_core::types::error::Temm1eError::Tool(format!("Ollama pull: {}", e))
            })?;

        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            return Err(temm1e_core::types::error::Temm1eError::Tool(format!(
                "Ollama pull error: {}",
                err
            )));
        }

        tracing::info!(model = model, "Eigen-Tune: embedding model ready");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_is_available_no_ollama() {
        // Ollama likely not running in test environment
        // This should return false without panicking
        let _ = is_available().await;
    }

    #[test]
    fn test_model_list_deserialize() {
        let json = r#"{"models":[{"name":"llama3.2:latest","size":4700000000}]}"#;
        let list: OllamaModelList = serde_json::from_str(json).unwrap();
        assert_eq!(list.models.len(), 1);
        assert_eq!(list.models[0].name, "llama3.2:latest");
    }

    #[test]
    fn test_embed_response_deserialize() {
        let json = r#"{"embeddings":[[0.1, 0.2, 0.3]]}"#;
        let resp: OllamaEmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embeddings[0].len(), 3);
    }
}
