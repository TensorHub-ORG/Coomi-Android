use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OpenAiCompatible,
    OpenAiResponses,
    AnthropicMessages,
    GeminiNative,
    /// Retained so Android-authored provider files stay readable. The desktop
    /// runtime reports a clear error because the private account protocol needs
    /// the mobile login flow.
    DeepseekAccount,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCompactionMode {
    Legacy,
    #[default]
    V2,
}

impl ProviderKind {
    fn from_config(provider_type: &str, tool_protocol: Option<&str>) -> Result<Self> {
        let provider_type = provider_type.trim();
        // Older coomi-rs builds incorrectly stored the API wire protocol in
        // `tool_protocol`. Keep those files readable, but do not let the new
        // tool-calling preference override the selected API compatibility.
        let legacy_protocol = tool_protocol.filter(|value| {
            matches!(
                value
                    .trim()
                    .to_ascii_lowercase()
                    .replace(['-', ' '], "_")
                    .as_str(),
                "openai_compatible" | "openai_responses" | "anthropic_messages" | "gemini_native"
            )
        });
        let value = if provider_type.is_empty() || provider_type.eq_ignore_ascii_case("generic") {
            legacy_protocol.unwrap_or(provider_type)
        } else {
            provider_type
        }
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
        match value.as_str() {
            "generic" | "deepseek" | "openai_compatible" | "chat_completions" => {
                Ok(Self::OpenAiCompatible)
            }
            "openai" | "openai_responses" | "responses" => Ok(Self::OpenAiResponses),
            "anthropic" | "anthropic_messages" => Ok(Self::AnthropicMessages),
            "gemini" | "gemini_native" => Ok(Self::GeminiNative),
            "deepseek_account" | "deepseek_login" => Ok(Self::DeepseekAccount),
            other => anyhow::bail!("unsupported provider protocol: {other}"),
        }
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    pub display: String,
    pub api_key: String,
    pub api_keys: Vec<String>,
    pub base_url: String,
    pub model: String,
    pub fast_model: Option<String>,
    /// Models declared by the provider editor, used to validate `provider:model`.
    pub models: Vec<String>,
    /// Per-model context window overrides.
    pub model_context_windows: BTreeMap<String, u64>,
    /// Per-model vision capability overrides.
    pub model_vision_support: BTreeMap<String, bool>,
    /// Per-model sampling parameters and reasoning mappings.
    pub model_parameters: BTreeMap<String, Value>,
    pub capabilities: coomi_engine::ModelCapabilities,
    pub remote_compaction_mode: RemoteCompactionMode,
    /// Additional request headers (for example gateway verification headers).
    pub extra_headers: BTreeMap<String, String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("display", &self.display)
            .field("api_key", &"[redacted]")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("fast_model", &self.fast_model)
            .field("model_context_windows", &self.model_context_windows)
            .field("model_vision_support", &self.model_vision_support)
            .field("model_parameters", &self.model_parameters)
            .field("capabilities", &self.capabilities)
            .field("remote_compaction_mode", &self.remote_compaction_mode)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    pub selector: String,
    pub provider_id: String,
    pub provider_display: String,
    pub model: String,
    pub is_fast: bool,
}

#[derive(Debug)]
pub struct ProviderRegistry {
    active: String,
    providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ProviderDocument {
    #[serde(default)]
    pub active: String,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSettings>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ProviderSettings {
    #[serde(rename = "type", default = "default_provider_type")]
    pub provider_type: String,
    #[serde(default)]
    pub tool_protocol: Option<String>,
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub fast_model: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub model_context_windows: BTreeMap<String, u64>,
    #[serde(default)]
    pub effective_context_window_percent: Option<u8>,
    #[serde(default)]
    pub auto_compact_token_limit: Option<u64>,
    #[serde(default)]
    pub auto_compact_scope: coomi_engine::AutoCompactScope,
    #[serde(default)]
    pub comp_hash: Option<String>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub supports_remote_compaction: Option<bool>,
    #[serde(default)]
    pub remote_compaction_mode: RemoteCompactionMode,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default = "default_true")]
    pub supports_native_tools: bool,
    #[serde(default)]
    pub supports_web_search: bool,
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn default_provider_type() -> String {
    "openai_compatible".into()
}

const fn default_true() -> bool {
    true
}

impl ProviderRegistry {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = ProviderDocument::load(path)?;
        let mut providers = BTreeMap::new();
        for (id, provider) in raw.providers {
            // Draft providers (no model selected yet) stay in the file but are
            // excluded from the runtime registry until the user completes them.
            if provider.model.trim().is_empty() {
                continue;
            }
            if provider.base_url.trim().is_empty() {
                anyhow::bail!("provider `{id}` has no base_url");
            }
            let kind = ProviderKind::from_config(
                &provider.provider_type,
                provider.tool_protocol.as_deref(),
            )?;
            let display = if provider.display.trim().is_empty() {
                id.clone()
            } else {
                provider.display
            };
            let mut model_vision_support = provider
                .extra
                .get("capabilityOverrides")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(model, value)| {
                    value
                        .get("vision")
                        .and_then(Value::as_bool)
                        .map(|enabled| (model.clone(), enabled))
                })
                .collect::<BTreeMap<_, _>>();
            for model in provider
                .extra
                .get("models")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                model_vision_support
                    .entry(model.to_owned())
                    .or_insert(provider.supports_vision);
            }
            model_vision_support
                .entry(provider.model.clone())
                .or_insert(provider.supports_vision);
            let manual_vision = model_vision_support
                .get(&provider.model)
                .copied()
                .unwrap_or(provider.supports_vision);
            let native_tools_enabled = provider.supports_native_tools
                && provider
                    .tool_protocol
                    .as_deref()
                    .is_none_or(|protocol| !protocol.eq_ignore_ascii_case("disabled"));
            let mut capabilities = coomi_engine::ModelCapabilities {
                context_window: provider.context_window.unwrap_or(256_000),
                effective_context_window_percent: provider
                    .effective_context_window_percent
                    .unwrap_or(95)
                    .clamp(1, 100),
                auto_compact_token_limit: provider.auto_compact_token_limit,
                auto_compact_scope: provider.auto_compact_scope,
                comp_hash: provider.comp_hash,
                max_output_tokens: provider.max_output_tokens.unwrap_or(8_192),
                supports_remote_compaction: provider
                    .supports_remote_compaction
                    .unwrap_or(kind == ProviderKind::OpenAiResponses),
                supports_vision: manual_vision,
                supports_native_tools: native_tools_enabled,
                supports_web_search: provider.supports_web_search,
                supports_parallel_tool_calls: provider.supports_parallel_tool_calls,
            };
            if let Some(window) = provider.model_context_windows.get(&provider.model) {
                capabilities.context_window = *window;
            }
            providers.insert(
                id.clone(),
                ProviderConfig {
                    id,
                    kind,
                    display,
                    api_key: provider.api_key,
                    api_keys: provider.api_keys.clone(),
                    base_url: provider.base_url,
                    model: provider.model,
                    fast_model: provider.fast_model.filter(|value| !value.trim().is_empty()),
                    models: provider
                        .extra
                        .get("models")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                    model_context_windows: provider.model_context_windows,
                    model_vision_support,
                    model_parameters: provider
                        .extra
                        .get("modelParameters")
                        .and_then(Value::as_object)
                        .map(|parameters| {
                            parameters
                                .iter()
                                .map(|(model, value)| (model.clone(), value.clone()))
                                .collect()
                        })
                        .unwrap_or_default(),
                    capabilities,
                    remote_compaction_mode: provider.remote_compaction_mode,
                    extra_headers: provider
                        .extra
                        .get("headers")
                        .and_then(Value::as_object)
                        .map(|headers| {
                            headers
                                .iter()
                                .filter_map(|(key, value)| {
                                    value.as_str().map(|value| (key.clone(), value.to_string()))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                },
            );
        }
        if providers.is_empty() {
            anyhow::bail!("provider file contains no providers")
        }
        let active = if raw.active.is_empty() {
            providers
                .keys()
                .next()
                .cloned()
                .context("provider file contains no providers")?
        } else {
            raw.active
        };
        if !providers.contains_key(&active) {
            anyhow::bail!("active provider `{active}` does not exist")
        }
        Ok(Self { active, providers })
    }

    pub fn active_id(&self) -> &str {
        &self.active
    }

    pub fn choices(&self) -> Vec<ModelChoice> {
        let mut choices = Vec::new();
        for provider in self.providers.values() {
            choices.push(ModelChoice {
                selector: provider.id.clone(),
                provider_id: provider.id.clone(),
                provider_display: provider.display.clone(),
                model: provider.model.clone(),
                is_fast: false,
            });
            if let Some(fast_model) = &provider.fast_model
                && fast_model != &provider.model
            {
                choices.push(ModelChoice {
                    selector: format!("{}:{fast_model}", provider.id),
                    provider_id: provider.id.clone(),
                    provider_display: provider.display.clone(),
                    model: fast_model.clone(),
                    is_fast: true,
                });
            }
        }
        choices
    }

    pub fn resolve(&self, selector: Option<&str>) -> Result<ProviderConfig> {
        let selector = selector.unwrap_or(&self.active).trim();
        if let Some(provider) = self.find_provider(selector) {
            return Ok(provider.clone());
        }

        for choice in self.choices() {
            if choice.selector.eq_ignore_ascii_case(selector)
                || choice.model.eq_ignore_ascii_case(selector)
            {
                let mut provider = self
                    .providers
                    .get(&choice.provider_id)
                    .context("model choice references a missing provider")?
                    .clone();
                provider.model = choice.model;
                apply_model_overrides(&mut provider);
                return Ok(provider);
            }
        }

        if let Some((provider_id, model)) = selector.split_once(':')
            && let Some(provider) = self.find_provider(provider_id)
        {
            let in_models = provider
                .models
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(model));
            let allowed = provider.model.eq_ignore_ascii_case(model)
                || provider
                    .fast_model
                    .as_deref()
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(model))
                || in_models;
            if allowed {
                let mut provider = provider.clone();
                provider.model = model.to_string();
                apply_model_overrides(&mut provider);
                return Ok(provider);
            }
            anyhow::bail!(
                "model `{model}` is not declared for provider `{}`",
                provider.id
            )
        }

        anyhow::bail!("model selector `{selector}` is not present in providers.json")
    }

    fn find_provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.get(id).or_else(|| {
            self.providers
                .values()
                .find(|provider| provider.id.eq_ignore_ascii_case(id))
        })
    }
}

fn apply_model_overrides(provider: &mut ProviderConfig) {
    if let Some(window) = provider.model_context_windows.get(&provider.model) {
        provider.capabilities.context_window = *window;
    }
    if let Some(enabled) = provider.model_vision_support.get(&provider.model) {
        provider.capabilities.supports_vision = *enabled;
    }
}

impl ProviderDocument {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read provider file {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid provider file {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("failed to save provider file {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        if !self.active.is_empty() {
            anyhow::ensure!(
                self.providers.contains_key(&self.active),
                "active provider `{}` does not exist",
                self.active
            );
        }
        for (id, provider) in &self.providers {
            anyhow::ensure!(!id.trim().is_empty(), "provider ID must not be empty");
            // Inactive providers may be drafts until the user completes them.
            if !self.active.is_empty() && id == &self.active {
                anyhow::ensure!(
                    !provider.model.trim().is_empty(),
                    "active provider `{id}` has no model"
                );
                anyhow::ensure!(
                    !provider.base_url.trim().is_empty(),
                    "active provider `{id}` has no base_url"
                );
            }
            ProviderKind::from_config(&provider.provider_type, provider.tool_protocol.as_deref())?;
        }
        Ok(())
    }
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            provider_type: default_provider_type(),
            tool_protocol: Some("auto".into()),
            display: String::new(),
            api_key: String::new(),
            api_keys: Vec::new(),
            base_url: String::new(),
            model: String::new(),
            fast_model: None,
            context_window: None,
            model_context_windows: BTreeMap::new(),
            effective_context_window_percent: None,
            auto_compact_token_limit: None,
            auto_compact_scope: coomi_engine::AutoCompactScope::Total,
            comp_hash: None,
            max_output_tokens: None,
            supports_remote_compaction: None,
            remote_compaction_mode: RemoteCompactionMode::default(),
            supports_vision: false,
            supports_native_tools: true,
            supports_web_search: false,
            supports_parallel_tool_calls: false,
            extra: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_only_include_models_declared_by_providers() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        fs::write(
            &path,
            r#"{
                "active": "primary",
                "providers": {
                    "primary": {
                        "type": "generic",
                        "display": "Primary",
                        "api_key": "secret",
                        "base_url": "https://example.test/v1",
                        "model": "main-model",
                        "fast_model": "fast-model"
                    }
                }
            }"#,
        )
        .expect("write provider fixture");
        let registry = ProviderRegistry::load(&path).expect("provider registry");
        assert_eq!(registry.choices().len(), 2);
        assert_eq!(
            registry
                .resolve(Some("primary:fast-model"))
                .expect("fast model")
                .model,
            "fast-model"
        );
        assert!(registry.resolve(Some("invented-model")).is_err());
        assert_eq!(
            registry
                .resolve(Some("primary"))
                .expect("primary")
                .capabilities
                .effective_context_window(),
            243_200
        );
    }

    #[test]
    fn declared_extra_models_are_resolvable_and_apply_overrides() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        fs::write(
            &path,
            r#"{
                "active": "primary",
                "providers": {
                    "primary": {
                        "type": "openai_compatible",
                        "base_url": "https://example.test/v1",
                        "model": "main-model",
                        "models": ["main-model", "alt-model"],
                        "model_context_windows": {"alt-model": 64000},
                        "capabilityOverrides": {"alt-model": {"vision": false}}
                    }
                }
            }"#,
        )
        .expect("write provider fixture");
        let registry = ProviderRegistry::load(&path).expect("provider registry");
        let provider = registry
            .resolve(Some("primary:alt-model"))
            .expect("declared model");
        assert_eq!(provider.model, "alt-model");
        assert_eq!(provider.capabilities.context_window, 64_000);
        assert!(!provider.capabilities.supports_vision);
    }

    #[test]
    fn draft_providers_are_skipped_and_active_must_be_complete() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        fs::write(
            &path,
            r#"{
                "active": "primary",
                "providers": {
                    "draft": {"type": "openai_compatible"},
                    "primary": {
                        "type": "openai_compatible",
                        "base_url": "https://example.test/v1",
                        "model": "main-model"
                    }
                }
            }"#,
        )
        .expect("write provider fixture");
        let registry = ProviderRegistry::load(&path).expect("provider registry");
        assert_eq!(registry.choices().len(), 1);
    }

    #[test]
    fn tool_protocol_does_not_override_api_kind_and_disabled_tools_are_honored() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("providers.json");
        fs::write(
            &path,
            r#"{
                "active": "primary",
                "providers": {
                    "primary": {
                        "type": "anthropic_messages",
                        "tool_protocol": "disabled",
                        "base_url": "https://example.test",
                        "model": "claude"
                    }
                }
            }"#,
        )
        .expect("write provider fixture");
        let registry = ProviderRegistry::load(&path).expect("provider registry");
        let provider = registry.resolve(None).expect("active provider");
        assert_eq!(provider.kind, ProviderKind::AnthropicMessages);
        assert!(!provider.capabilities.supports_native_tools);
    }
}
