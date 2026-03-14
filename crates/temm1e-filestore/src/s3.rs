//! S3-compatible file store implementation.
//!
//! Supports AWS S3, Cloudflare R2, MinIO, and any S3-compatible object storage.
//! Gated behind the `s3` feature flag.

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::time::Duration;
use temm1e_core::error::Temm1eError;
use temm1e_core::file::FileMetadata;
use temm1e_core::types::config::FileStoreConfig;
use temm1e_core::FileStore;
use tracing::{debug, info};

/// An S3-compatible file store (AWS S3, Cloudflare R2, MinIO, etc.).
#[derive(Debug)]
pub struct S3FileStore {
    client: Client,
    bucket: String,
}

impl S3FileStore {
    /// Create a new `S3FileStore` from a [`FileStoreConfig`].
    ///
    /// The config must include a `bucket` name. Optionally, `region` and
    /// `endpoint` can be set for non-AWS S3-compatible services (e.g. R2, MinIO).
    pub async fn new(config: &FileStoreConfig) -> Result<Self, Temm1eError> {
        let bucket = config.bucket.clone().ok_or_else(|| {
            Temm1eError::FileTransfer("S3 file store requires a 'bucket' in config".to_string())
        })?;

        let sdk_config = aws_config::defaults(BehaviorVersion::latest()).load().await;

        let mut s3_config_builder = S3ConfigBuilder::from(&sdk_config);

        if let Some(ref region) = config.region {
            s3_config_builder =
                s3_config_builder.region(aws_sdk_s3::config::Region::new(region.clone()));
        }

        if let Some(ref endpoint) = config.endpoint {
            s3_config_builder = s3_config_builder
                .endpoint_url(endpoint)
                .force_path_style(true);
        }

        let s3_config = s3_config_builder.build();
        let client = Client::from_conf(s3_config);

        info!(bucket = %bucket, "S3 file store initialised");

        Ok(Self { client, bucket })
    }
}

#[async_trait]
impl FileStore for S3FileStore {
    async fn store(
        &self,
        path: &str,
        data: Bytes,
        metadata: FileMetadata,
    ) -> Result<String, Temm1eError> {
        let body = ByteStream::from(data);

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(path)
            .body(body)
            .content_type(&metadata.mime_type)
            .send()
            .await
            .map_err(|e| {
                Temm1eError::FileTransfer(format!("S3 PutObject failed for {path}: {e}"))
            })?;

        debug!(
            bucket = %self.bucket,
            key = %path,
            name = %metadata.name,
            mime_type = %metadata.mime_type,
            size = ?metadata.size,
            "Stored file in S3"
        );

        Ok(path.to_string())
    }

    async fn store_stream(
        &self,
        path: &str,
        mut stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<String, Temm1eError> {
        // For the streaming upload, we use S3 multipart upload.
        let create = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(path)
            .content_type(&metadata.mime_type)
            .send()
            .await
            .map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "S3 CreateMultipartUpload failed for {path}: {e}"
                ))
            })?;

        let upload_id = create.upload_id().ok_or_else(|| {
            Temm1eError::FileTransfer("S3 CreateMultipartUpload returned no upload_id".to_string())
        })?;

        let mut parts = Vec::new();
        let mut part_number: i32 = 1;
        // S3 requires minimum 5 MB parts (except the last). Buffer chunks.
        const MIN_PART_SIZE: usize = 5 * 1024 * 1024; // 5 MB
        let mut buffer = Vec::new();

        while let Some(chunk) = stream.next().await {
            buffer.extend_from_slice(&chunk);

            if buffer.len() >= MIN_PART_SIZE {
                let body = ByteStream::from(Bytes::from(std::mem::take(&mut buffer)));
                let upload_part = self
                    .client
                    .upload_part()
                    .bucket(&self.bucket)
                    .key(path)
                    .upload_id(upload_id)
                    .part_number(part_number)
                    .body(body)
                    .send()
                    .await
                    .map_err(|e| {
                        Temm1eError::FileTransfer(format!(
                            "S3 UploadPart {part_number} failed for {path}: {e}"
                        ))
                    })?;

                let completed_part = aws_sdk_s3::types::CompletedPart::builder()
                    .part_number(part_number)
                    .set_e_tag(upload_part.e_tag().map(|s| s.to_string()))
                    .build();

                parts.push(completed_part);
                part_number += 1;
            }
        }

        // Upload the remaining buffer as the final part.
        if !buffer.is_empty() || parts.is_empty() {
            let body = ByteStream::from(Bytes::from(buffer));
            let upload_part = self
                .client
                .upload_part()
                .bucket(&self.bucket)
                .key(path)
                .upload_id(upload_id)
                .part_number(part_number)
                .body(body)
                .send()
                .await
                .map_err(|e| {
                    Temm1eError::FileTransfer(format!(
                        "S3 UploadPart (final) {part_number} failed for {path}: {e}"
                    ))
                })?;

            let completed_part = aws_sdk_s3::types::CompletedPart::builder()
                .part_number(part_number)
                .set_e_tag(upload_part.e_tag().map(|s| s.to_string()))
                .build();

            parts.push(completed_part);
        }

        let completed_upload = aws_sdk_s3::types::CompletedMultipartUpload::builder()
            .set_parts(Some(parts))
            .build();

        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(path)
            .upload_id(upload_id)
            .multipart_upload(completed_upload)
            .send()
            .await
            .map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "S3 CompleteMultipartUpload failed for {path}: {e}"
                ))
            })?;

        debug!(
            bucket = %self.bucket,
            key = %path,
            name = %metadata.name,
            "Stored streamed file in S3 via multipart upload"
        );

        Ok(path.to_string())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, Temm1eError> {
        let result = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;

        match result {
            Ok(output) => {
                let data = output
                    .body
                    .collect()
                    .await
                    .map_err(|e| {
                        Temm1eError::FileTransfer(format!(
                            "Failed to read S3 object body for {key}: {e}"
                        ))
                    })?
                    .into_bytes();

                debug!(bucket = %self.bucket, key = %key, size = data.len(), "Retrieved file from S3");
                Ok(Some(data))
            }
            Err(sdk_err) => {
                // Check for NoSuchKey or 404.
                let service_err = sdk_err.into_service_error();
                if service_err.is_no_such_key() {
                    debug!(bucket = %self.bucket, key = %key, "File not found in S3");
                    Ok(None)
                } else {
                    Err(Temm1eError::FileTransfer(format!(
                        "S3 GetObject failed for {key}: {service_err}"
                    )))
                }
            }
        }
    }

    async fn presigned_url(
        &self,
        key: &str,
        expires_in_secs: u64,
    ) -> Result<Option<String>, Temm1eError> {
        let presign_config = PresigningConfig::expires_in(Duration::from_secs(expires_in_secs))
            .map_err(|e| Temm1eError::FileTransfer(format!("Invalid presigning duration: {e}")))?;

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(presign_config)
            .await
            .map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "Failed to generate presigned URL for {key}: {e}"
                ))
            })?;

        let url = presigned.uri().to_string();
        debug!(bucket = %self.bucket, key = %key, expires_in_secs = expires_in_secs, "Generated presigned URL");
        Ok(Some(url))
    }

    async fn delete(&self, key: &str) -> Result<(), Temm1eError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                Temm1eError::FileTransfer(format!("S3 DeleteObject failed for {key}: {e}"))
            })?;

        debug!(bucket = %self.bucket, key = %key, "Deleted file from S3");
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, Temm1eError> {
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut request = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);

            if let Some(ref token) = continuation_token {
                request = request.continuation_token(token);
            }

            let output = request.send().await.map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "S3 ListObjectsV2 failed for prefix {prefix}: {e}"
                ))
            })?;

            for object in output.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_string());
                }
            }

            if output.is_truncated() == Some(true) {
                continuation_token = output.next_continuation_token().map(|s| s.to_string());
            } else {
                break;
            }
        }

        debug!(bucket = %self.bucket, prefix = %prefix, count = keys.len(), "Listed files in S3");
        Ok(keys)
    }

    fn backend_name(&self) -> &str {
        "s3"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temm1e_core::types::config::FileStoreConfig;

    #[test]
    fn test_backend_name() {
        // We can't construct a real S3FileStore without AWS credentials,
        // but we can test config parsing and error handling.
        let config = FileStoreConfig {
            backend: "s3".to_string(),
            bucket: Some("test-bucket".to_string()),
            region: Some("us-east-1".to_string()),
            endpoint: None,
            path: None,
        };
        assert_eq!(config.backend, "s3");
        assert_eq!(config.bucket.as_deref(), Some("test-bucket"));
    }

    #[tokio::test]
    async fn test_missing_bucket_returns_error() {
        let config = FileStoreConfig {
            backend: "s3".to_string(),
            bucket: None,
            region: Some("us-east-1".to_string()),
            endpoint: None,
            path: None,
        };

        let result = S3FileStore::new(&config).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("bucket"),
            "Error should mention missing bucket: {err}"
        );
    }

    #[test]
    fn test_config_with_endpoint() {
        let config = FileStoreConfig {
            backend: "s3".to_string(),
            bucket: Some("my-r2-bucket".to_string()),
            region: Some("auto".to_string()),
            endpoint: Some("https://account-id.r2.cloudflarestorage.com".to_string()),
            path: None,
        };
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://account-id.r2.cloudflarestorage.com")
        );
        assert_eq!(config.region.as_deref(), Some("auto"));
    }

    #[test]
    fn test_config_defaults() {
        let config = FileStoreConfig::default();
        assert_eq!(config.backend, "local");
        assert!(config.bucket.is_none());
        assert!(config.region.is_none());
        assert!(config.endpoint.is_none());
        assert!(config.path.is_none());
    }

    #[test]
    fn test_config_deserialization() {
        let toml_str = r#"
            backend = "s3"
            bucket = "my-bucket"
            region = "eu-west-1"
            endpoint = "https://s3.example.com"
        "#;
        let config: FileStoreConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, "s3");
        assert_eq!(config.bucket.as_deref(), Some("my-bucket"));
        assert_eq!(config.region.as_deref(), Some("eu-west-1"));
        assert_eq!(config.endpoint.as_deref(), Some("https://s3.example.com"));
    }

    #[test]
    fn test_config_deserialization_minimal() {
        let toml_str = r#"
            backend = "s3"
            bucket = "my-bucket"
        "#;
        let config: FileStoreConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.backend, "s3");
        assert_eq!(config.bucket.as_deref(), Some("my-bucket"));
        assert!(config.region.is_none());
        assert!(config.endpoint.is_none());
    }
}
