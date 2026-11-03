use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CAPABILITY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAiCompatible,
    OpenAiResponses,
    Anthropic,
    Gemini,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Verified,
    Inferred,
    Unsupported,
    Unknown,
}

impl CapabilityState {
    pub fn supported(self) -> bool {
        matches!(self, Self::Verified | Self::Inferred)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityEvidence {
    pub state: CapabilityState,
    pub checked_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CapabilityEvidence {
    pub fn inferred() -> Self {
        Self {
            state: CapabilityState::Inferred,
            checked_at_ms: now_ms(),
            error: None,
        }
    }

    pub fn unknown(error: Option<String>) -> Self {
        Self {
            state: CapabilityState::Unknown,
            checked_at_ms: now_ms(),
            error: error.map(redact_error),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CapabilityCacheKey {
    pub provider: String,
    pub model: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub key_version_fingerprint: String,
}

impl CapabilityCacheKey {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        protocol: ProviderProtocol,
        base_url: &str,
        api_key: &str,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            protocol,
            base_url: EndpointResolver::new(base_url, protocol).normalized_base,
            key_version_fingerprint: secret_fingerprint(api_key),
        }
    }

    fn storage_key(&self) -> String {
        let serialized = serde_json::to_vec(self).unwrap_or_default();
        sha256_hex(&serialized)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelCapabilityProfile {
    pub key: CapabilityCacheKey,
    pub text: CapabilityEvidence,
    pub vision: CapabilityEvidence,
    pub image_generation: CapabilityEvidence,
    pub native_tools: CapabilityEvidence,
    pub parallel_tools: CapabilityEvidence,
    pub web_search: CapabilityEvidence,
    pub streaming: CapabilityEvidence,
    #[serde(default)]
    pub reasoning_efforts: BTreeSet<String>,
    pub source: String,
    pub probed_at_ms: u64,
}

impl ModelCapabilityProfile {
    pub fn inferred(key: CapabilityCacheKey) -> Self {
        let model = key.model.to_ascii_lowercase();
        let protocol = key.protocol;
        let text = CapabilityEvidence::inferred();
        let vision = if [
            "vision",
            "gpt-4o",
            "gemini",
            "claude-3",
            "claude-sonnet",
            "claude-opus",
        ]
        .iter()
        .any(|marker| model.contains(marker))
        {
            CapabilityEvidence::inferred()
        } else {
            CapabilityEvidence::unknown(None)
        };
        let image_generation = if ["dall-e", "gpt-image", "imagen"]
            .iter()
            .any(|marker| model.contains(marker))
        {
            CapabilityEvidence::inferred()
        } else {
            CapabilityEvidence::unknown(None)
        };
        let native_tools = if matches!(
            protocol,
            ProviderProtocol::OpenAiCompatible
                | ProviderProtocol::OpenAiResponses
                | ProviderProtocol::Anthropic
                | ProviderProtocol::Gemini
        ) {
            CapabilityEvidence::inferred()
        } else {
            CapabilityEvidence::unknown(None)
        };
        let mut reasoning_efforts = BTreeSet::new();
        if model.contains("gpt-5") || model.starts_with('o') {
            reasoning_efforts.extend(["low", "medium", "high", "xhigh"].map(str::to_owned));
        } else if model.contains("reason") || model.contains("thinking") {
            reasoning_efforts.extend(["low", "medium", "high"].map(str::to_owned));
        }
        Self {
            key,
            text,
            vision,
            image_generation,
            native_tools,
            parallel_tools: CapabilityEvidence::unknown(None),
            web_search: CapabilityEvidence::unknown(None),
            streaming: CapabilityEvidence::inferred(),
            reasoning_efforts,
            source: "model-name-and-protocol".into(),
            probed_at_ms: now_ms(),
        }
    }

    pub fn mark_verified(&mut self, capability: &str, supported: bool) -> Result<()> {
        let evidence = CapabilityEvidence {
            state: if supported {
                CapabilityState::Verified
            } else {
                CapabilityState::Unsupported
            },
            checked_at_ms: now_ms(),
            error: None,
        };
        match capability {
            "text" => self.text = evidence,
            "vision" => self.vision = evidence,
            "image_generation" => self.image_generation = evidence,
            "native_tools" => self.native_tools = evidence,
            "parallel_tools" => self.parallel_tools = evidence,
            "web_search" => self.web_search = evidence,
            "streaming" => self.streaming = evidence,
            _ => anyhow::bail!("unknown model capability `{capability}`"),
        }
        self.probed_at_ms = now_ms();
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CapabilityDocument {
    version: u32,
    profiles: BTreeMap<String, ModelCapabilityProfile>,
}

impl Default for CapabilityDocument {
    fn default() -> Self {
        Self {
            version: CAPABILITY_SCHEMA_VERSION,
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CapabilityRegistry {
    path: PathBuf,
    document: CapabilityDocument,
}

impl CapabilityRegistry {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("config").join("provider_capabilities.json");
        let document = match fs::read(&path) {
            Ok(bytes) => {
                let parsed: CapabilityDocument = serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid capability cache {}", path.display()))?;
                if parsed.version == CAPABILITY_SCHEMA_VERSION {
                    parsed
                } else {
                    CapabilityDocument::default()
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                CapabilityDocument::default()
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self { path, document })
    }

    pub fn get(&self, key: &CapabilityCacheKey) -> Option<&ModelCapabilityProfile> {
        self.document.profiles.get(&key.storage_key())
    }

    pub fn upsert(&mut self, profile: ModelCapabilityProfile) -> Result<()> {
        self.document
            .profiles
            .insert(profile.key.storage_key(), profile);
        self.save()
    }

    pub fn get_or_infer(&mut self, key: CapabilityCacheKey) -> Result<ModelCapabilityProfile> {
        if let Some(profile) = self.get(&key) {
            return Ok(profile.clone());
        }
        let profile = ModelCapabilityProfile::inferred(key);
        self.upsert(profile.clone())?;
        Ok(profile)
    }

    pub fn invalidate_provider(&mut self, provider: &str) -> Result<usize> {
        let before = self.document.profiles.len();
        self.document
            .profiles
            .retain(|_, profile| profile.key.provider != provider);
        let removed = before.saturating_sub(self.document.profiles.len());
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn profiles_for_provider(&self, provider: &str) -> Vec<ModelCapabilityProfile> {
        self.document
            .profiles
            .values()
            .filter(|profile| profile.key.provider == provider)
            .cloned()
            .collect()
    }

    fn save(&self) -> Result<()> {
        let parent = self
            .path
            .parent()
            .context("capability path has no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("json.tmp");
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, &self.document)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temporary, &self.path)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointResolver {
    pub normalized_base: String,
    protocol: ProviderProtocol,
}

impl EndpointResolver {
    pub fn new(base_url: &str, protocol: ProviderProtocol) -> Self {
        let mut normalized = base_url.trim().trim_end_matches('/').to_owned();
        while normalized.ends_with("/v1/v1") {
            normalized.truncate(normalized.len().saturating_sub(3));
        }
        Self {
            normalized_base: normalized,
            protocol,
        }
    }

    pub fn models(&self) -> String {
        match self.protocol {
            ProviderProtocol::Gemini => self.join_versioned("v1beta/models", "models"),
            _ => self.join_versioned("v1/models", "models"),
        }
    }

    pub fn inference(&self, model: &str) -> String {
        match self.protocol {
            ProviderProtocol::OpenAiCompatible => {
                self.join_versioned("v1/chat/completions", "chat/completions")
            }
            ProviderProtocol::OpenAiResponses => self.join_versioned("v1/responses", "responses"),
            ProviderProtocol::Anthropic => self.join_versioned("v1/messages", "messages"),
            ProviderProtocol::Gemini => {
                let root = self.join_versioned("v1beta", "");
                format!("{root}/models/{model}:generateContent")
            }
        }
    }

    fn join_versioned(&self, default_suffix: &str, existing_version_suffix: &str) -> String {
        let last = self
            .normalized_base
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let suffix = if matches!(last.as_str(), "v1" | "v1beta") {
            existing_version_suffix
        } else {
            default_suffix
        };
        if suffix.is_empty() {
            self.normalized_base.clone()
        } else {
            format!(
                "{}/{}",
                self.normalized_base,
                suffix.trim_start_matches('/')
            )
        }
    }
}

pub fn resolve_reasoning_effort(
    requested: &str,
    supported: &BTreeSet<String>,
    prompt_chars: usize,
) -> Option<String> {
    if supported.is_empty() {
        return None;
    }
    let desired = if requested == "auto" {
        match prompt_chars {
            0..=240 => "low",
            241..=2_000 => "medium",
            2_001..=8_000 => "high",
            _ => "xhigh",
        }
    } else {
        requested
    };
    let levels = ["low", "medium", "high", "xhigh"];
    let desired_index = levels.iter().position(|item| *item == desired).unwrap_or(1);
    levels
        .iter()
        .enumerate()
        .filter(|(_, level)| supported.contains(**level))
        .min_by_key(|(index, _)| index.abs_diff(desired_index))
        .map(|(_, level)| (*level).to_owned())
}

pub fn prune_unsupported_fields(payload: &mut Value, profile: &ModelCapabilityProfile) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    if !profile.native_tools.state.supported() {
        object.remove("tools");
        object.remove("tool_choice");
    }
    if !profile.parallel_tools.state.supported() {
        object.remove("parallel_tool_calls");
    }
    if profile.reasoning_efforts.is_empty() {
        object.remove("reasoning");
        object.remove("reasoning_effort");
        object.remove("thinking");
    }
    if !profile.web_search.state.supported() {
        object.remove("web_search_options");
    }
    if !profile.streaming.state.supported() {
        object.remove("stream");
    }
    if !profile.vision.state.supported() {
        strip_images(payload);
    }
}

pub async fn probe_model_capabilities(
    profile: ModelCapabilityProfile,
    api_key: &str,
) -> ModelCapabilityProfile {
    probe_model_capabilities_with_timeout(profile, api_key, std::time::Duration::from_secs(20))
        .await
}

async fn probe_model_capabilities_with_timeout(
    mut profile: ModelCapabilityProfile,
    api_key: &str,
    timeout: std::time::Duration,
) -> ModelCapabilityProfile {
    let resolver = EndpointResolver::new(&profile.key.base_url, profile.key.protocol);
    let endpoint = resolver.inference(&profile.key.model);
    let body = match profile.key.protocol {
        ProviderProtocol::OpenAiCompatible => json!({
            "model": profile.key.model,
            "messages": [{"role":"user","content":"Reply OK"}],
            "max_tokens": 1,
            "stream": false,
        }),
        ProviderProtocol::OpenAiResponses => json!({
            "model": profile.key.model,
            "input": "Reply OK",
            "max_output_tokens": 1,
            "stream": false,
        }),
        ProviderProtocol::Anthropic => json!({
            "model": profile.key.model,
            "messages": [{"role":"user","content":"Reply OK"}],
            "max_tokens": 1,
            "stream": false,
        }),
        ProviderProtocol::Gemini => json!({
            "contents": [{"role":"user","parts":[{"text":"Reply OK"}]}],
            "generationConfig": {"maxOutputTokens": 1},
        }),
    };
    let client = match reqwest::Client::builder()
        .connect_timeout(timeout.min(std::time::Duration::from_secs(10)))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            profile.text = CapabilityEvidence::unknown(Some(error.to_string()));
            return profile;
        }
    };
    let mut request = client.post(endpoint).json(&body);
    match profile.key.protocol {
        ProviderProtocol::Gemini => {
            request = request.query(&[("key", api_key)]);
        }
        ProviderProtocol::Anthropic => {
            request = request
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        }
        _ if !api_key.is_empty() => request = request.bearer_auth(api_key),
        _ => {}
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            profile.text = CapabilityEvidence::unknown(Some(if error.is_timeout() {
                "probe timed out".into()
            } else {
                error.to_string()
            }));
            profile.source = "active-probe".into();
            profile.probed_at_ms = now_ms();
            return profile;
        }
    };
    let status = response.status();
    let response_body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            profile.text = CapabilityEvidence::unknown(Some(error.to_string()));
            profile.source = "active-probe".into();
            profile.probed_at_ms = now_ms();
            return profile;
        }
    };
    if status.is_success() {
        if serde_json::from_str::<Value>(&response_body).is_ok() {
            let _ = profile.mark_verified("text", true);
            profile.streaming = CapabilityEvidence::inferred();
        } else {
            profile.text =
                CapabilityEvidence::unknown(Some("provider returned invalid JSON".into()));
        }
    } else {
        let summary = format!(
            "HTTP {status}: {}",
            response_body.chars().take(160).collect::<String>()
        );
        profile.text = CapabilityEvidence::unknown(Some(summary.clone()));
        let lower = response_body.to_ascii_lowercase();
        for (field, capability) in [
            ("parallel_tool_calls", "parallel_tools"),
            ("web_search", "web_search"),
            ("reasoning", "reasoning"),
            ("image", "vision"),
            ("tools", "native_tools"),
        ] {
            if status == reqwest::StatusCode::BAD_REQUEST && lower.contains(field) {
                if capability == "reasoning" {
                    profile.reasoning_efforts.clear();
                } else {
                    let _ = profile.mark_verified(capability, false);
                }
            }
        }
    }
    profile.source = "active-probe".into();
    profile.probed_at_ms = now_ms();
    profile
}

fn strip_images(value: &mut Value) {
    match value {
        Value::Array(items) => {
            items.retain(|item| {
                !item
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "image_url" | "input_image" | "image"))
            });
            for item in items {
                strip_images(item);
            }
        }
        Value::Object(object) => {
            object.remove("images");
            for item in object.values_mut() {
                strip_images(item);
            }
        }
        _ => {}
    }
}

fn secret_fingerprint(secret: &str) -> String {
    if secret.is_empty() {
        return "none".into();
    }
    sha256_hex(secret.as_bytes()).chars().take(16).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn redact_error(value: String) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("authorization") || lower.contains("api key") || lower.contains("bearer ") {
        "provider rejected credentials".into()
    } else {
        value.chars().take(240).collect()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn endpoint_resolver_handles_versioned_and_unversioned_bases() {
        let root =
            EndpointResolver::new("https://example.test", ProviderProtocol::OpenAiCompatible);
        let versioned = EndpointResolver::new(
            "https://example.test/v1/",
            ProviderProtocol::OpenAiCompatible,
        );
        assert_eq!(root.models(), "https://example.test/v1/models");
        assert_eq!(versioned.models(), "https://example.test/v1/models");
        assert_eq!(
            root.inference("m"),
            "https://example.test/v1/chat/completions"
        );
        assert_eq!(
            versioned.inference("m"),
            "https://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn endpoint_resolver_supports_all_protocols() {
        assert_eq!(
            EndpointResolver::new("https://a.test", ProviderProtocol::OpenAiResponses)
                .inference("m"),
            "https://a.test/v1/responses"
        );
        assert_eq!(
            EndpointResolver::new("https://a.test/v1", ProviderProtocol::Anthropic).inference("m"),
            "https://a.test/v1/messages"
        );
        assert_eq!(
            EndpointResolver::new("https://a.test", ProviderProtocol::Gemini).inference("gemini"),
            "https://a.test/v1beta/models/gemini:generateContent"
        );
    }

    #[test]
    fn cache_key_tracks_model_protocol_url_and_key_version() {
        let first = CapabilityCacheKey::new(
            "provider",
            "model",
            ProviderProtocol::OpenAiCompatible,
            "https://example.test/v1/",
            "one",
        );
        let second = CapabilityCacheKey::new(
            "provider",
            "model",
            ProviderProtocol::OpenAiCompatible,
            "https://example.test/v1",
            "two",
        );
        assert_eq!(first.base_url, second.base_url);
        assert_ne!(
            first.key_version_fingerprint,
            second.key_version_fingerprint
        );
    }

    #[test]
    fn maps_auto_and_missing_reasoning_levels() {
        let supported = ["low", "high"].map(str::to_owned).into_iter().collect();
        assert_eq!(
            resolve_reasoning_effort("auto", &supported, 600),
            Some("low".into())
        );
        assert_eq!(
            resolve_reasoning_effort("xhigh", &supported, 10_000),
            Some("high".into())
        );
    }

    #[test]
    fn prunes_fields_before_request_when_capabilities_are_unknown() {
        let key = CapabilityCacheKey::new(
            "provider",
            "plain-model",
            ProviderProtocol::OpenAiCompatible,
            "https://example.test",
            "key",
        );
        let mut profile = ModelCapabilityProfile::inferred(key);
        profile.native_tools.state = CapabilityState::Unsupported;
        profile.vision.state = CapabilityState::Unsupported;
        let mut payload = json!({
            "tools": [{"type":"function"}],
            "parallel_tool_calls": true,
            "reasoning_effort": "high",
            "messages": [{"content":[{"type":"text","text":"hello"},{"type":"image_url","image_url":{"url":"data:"}}]}]
        });
        prune_unsupported_fields(&mut payload, &profile);
        assert!(payload.get("tools").is_none());
        assert!(payload.get("parallel_tool_calls").is_none());
        assert!(payload.get("reasoning_effort").is_none());
        assert_eq!(
            payload["messages"][0]["content"].as_array().map(Vec::len),
            Some(1)
        );
    }

    async fn mock_server(status: &str, body: &str, delay_ms: u64) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock server");
        let address = listener.local_addr().expect("mock address");
        let status = status.to_owned();
        let body = body.to_owned();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept probe");
            let mut request = vec![0u8; 8192];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, response.as_bytes()).await;
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn probe_handles_success_field_rejection_rate_limit_bad_json_and_timeout() {
        let cases = [
            ("200 OK", "{}", 0, CapabilityState::Verified, None),
            (
                "400 Bad Request",
                r#"{"error":"parallel_tool_calls unsupported"}"#,
                0,
                CapabilityState::Unknown,
                Some(CapabilityState::Unsupported),
            ),
            (
                "429 Too Many Requests",
                r#"{"error":"slow down"}"#,
                0,
                CapabilityState::Unknown,
                None,
            ),
            ("200 OK", "not-json", 0, CapabilityState::Unknown, None),
            ("200 OK", "{}", 100, CapabilityState::Unknown, None),
        ];
        for (status, body, delay, text_state, parallel_state) in cases {
            let base = mock_server(status, body, delay).await;
            let key = CapabilityCacheKey::new(
                "mock",
                "model",
                ProviderProtocol::OpenAiCompatible,
                &base,
                "key",
            );
            let profile = probe_model_capabilities_with_timeout(
                ModelCapabilityProfile::inferred(key),
                "key",
                std::time::Duration::from_millis(40),
            )
            .await;
            assert_eq!(profile.text.state, text_state);
            if let Some(expected) = parallel_state {
                assert_eq!(profile.parallel_tools.state, expected);
            }
        }
    }

    #[tokio::test]
    async fn probe_targets_protocol_specific_versioned_endpoints() {
        for protocol in [
            ProviderProtocol::OpenAiCompatible,
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::Anthropic,
            ProviderProtocol::Gemini,
        ] {
            let base = mock_server("200 OK", "{}", 0).await;
            let key = CapabilityCacheKey::new("mock", "model", protocol, &base, "key");
            let profile =
                probe_model_capabilities(ModelCapabilityProfile::inferred(key), "key").await;
            assert_eq!(profile.text.state, CapabilityState::Verified);
        }
    }
}
