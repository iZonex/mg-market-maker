//! S3 client wrapper — encapsulates endpoint override, SSE
//! mode, and key prefixing so callers (`shipper`, `bundle`)
//! only touch a narrow API.

use aws_sdk_s3::config::Region;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ServerSideEncryption;
use aws_sdk_s3::Client;
use mm_common::config::ArchiveConfig;
use std::sync::Arc;
use std::time::Duration;

/// Built-once, cloned-around S3 client carrying the archive
/// config so upload callsites don't re-read TOML each time.
#[derive(Clone, Debug)]
pub struct ArchiveClient {
    inner: Client,
    cfg: Arc<ArchiveConfig>,
}

impl ArchiveClient {
    /// Build the SDK client from the archive config. The SDK's
    /// default credential provider chain is used unchanged —
    /// env, IMDS, SSO, profile all keep working.
    pub async fn from_config(cfg: ArchiveConfig) -> anyhow::Result<Self> {
        let region = Region::new(cfg.s3_region.clone());
        let mut loader = aws_config::defaults(aws_config::BehaviorVersion::latest()).region(region);
        if let Some(ep) = cfg.s3_endpoint_url.as_deref() {
            loader = loader.endpoint_url(ep);
        }
        let sdk_config = loader.load().await;

        // `force_path_style` is the safe default when an endpoint
        // override is set — MinIO + older R2 configs reject
        // virtual-hosted-style paths, which is aws-sdk-s3's
        // default. Real AWS accepts both.
        let s3_conf = aws_sdk_s3::config::Builder::from(&sdk_config)
            .force_path_style(cfg.s3_endpoint_url.is_some())
            .build();

        Ok(Self {
            inner: Client::from_conf(s3_conf),
            cfg: Arc::new(cfg),
        })
    }

    /// Bucket + fully-qualified key for a relative path.
    pub fn resolve_key(&self, rel: &str) -> String {
        let prefix = self.cfg.s3_prefix.trim_matches('/');
        if prefix.is_empty() {
            rel.trim_matches('/').to_string()
        } else {
            format!("{}/{}", prefix, rel.trim_matches('/'))
        }
    }

    /// Upload bytes under `rel` (relative to the configured
    /// prefix). Picks SSE-KMS when `encrypt_kms_key` is set,
    /// otherwise SSE-S3 (AES256).
    pub async fn put_object(
        &self,
        rel: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> anyhow::Result<String> {
        let key = self.resolve_key(rel);
        let mut req = self
            .inner
            .put_object()
            .bucket(&self.cfg.s3_bucket)
            .key(&key)
            .body(ByteStream::from(body))
            .content_type(content_type);

        match self.cfg.encrypt_kms_key.as_deref() {
            Some(kms_id) => {
                req = req
                    .server_side_encryption(ServerSideEncryption::AwsKms)
                    .ssekms_key_id(kms_id);
            }
            None => {
                req = req.server_side_encryption(ServerSideEncryption::Aes256);
            }
        }

        req.send()
            .await
            .map_err(|e| anyhow::anyhow!("s3 put failed: {e}"))?;
        Ok(key)
    }

    /// Presigned GET URL with a caller-chosen TTL. Used by the
    /// bundle endpoint when a client wants a time-limited link
    /// they can hand off to a regulator without issuing
    /// long-lived IAM creds.
    pub async fn presign_get(&self, rel: &str, ttl: Duration) -> anyhow::Result<String> {
        let key = self.resolve_key(rel);
        let presign = PresigningConfig::expires_in(ttl)?;
        let presigned = self
            .inner
            .get_object()
            .bucket(&self.cfg.s3_bucket)
            .key(&key)
            .presigned(presign)
            .await
            .map_err(|e| anyhow::anyhow!("presign failed: {e}"))?;
        Ok(presigned.uri().to_string())
    }

    pub fn config(&self) -> &ArchiveConfig {
        &self.cfg
    }

    /// Lightweight reachability probe — does HEAD on the
    /// configured bucket. Succeeds when the credentials,
    /// region, and (when applicable) endpoint override line
    /// up, without actually writing anything. Used by the
    /// `/api/v1/archive/health` endpoint so operators can
    /// verify the S3 path is wired before the first shipper
    /// tick ships — failures at 01:00 are much less useful
    /// than failures at boot.
    pub async fn health_check(&self) -> anyhow::Result<()> {
        self.inner
            .head_bucket()
            .bucket(&self.cfg.s3_bucket)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("s3 head_bucket failed: {e}"))?;
        Ok(())
    }
}
