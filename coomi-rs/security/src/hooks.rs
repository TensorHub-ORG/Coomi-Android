use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Clone, Copy, Debug)]
pub enum HookEvent {
    SessionStart,
    TurnStart,
    TurnEnd,
    PreToolUse,
    PostToolUse,
}

impl HookEvent {
    fn key(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::PreToolUse => "pre_tool_use",
            Self::PostToolUse => "post_tool_use",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct HookOutcome {
    #[serde(default = "default_true")]
    pub allow: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub arguments: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub additional_context: String,
}

#[derive(Clone, Debug, Deserialize)]
struct HookConfig {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_matcher")]
    matcher: String,
    #[serde(default)]
    keyword: String,
    #[serde(default)]
    keyword_match: KeywordMatch,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum KeywordMatch {
    #[default]
    Disabled,
    Exact,
    Contains,
}

#[derive(Default, Deserialize)]
struct HookDocument {
    #[serde(default)]
    hooks: BTreeMap<String, Vec<HookConfig>>,
}

#[derive(Clone, Default)]
pub struct HookRunner {
    hooks: BTreeMap<String, Vec<HookConfig>>,
}

impl HookRunner {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("config").join("hooks.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let document: HookDocument = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid hook file {}", path.display()))?;
        Ok(Self {
            hooks: document.hooks,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.values().all(Vec::is_empty)
    }

    pub async fn run(
        &self,
        event: HookEvent,
        subject: Option<&str>,
        payload: Value,
    ) -> Result<HookOutcome> {
        let mut aggregate = HookOutcome {
            allow: true,
            ..HookOutcome::default()
        };
        let Some(hooks) = self.hooks.get(event.key()) else {
            return Ok(aggregate);
        };
        for hook in hooks {
            if !hook.enabled {
                continue;
            }
            if !matches_subject(&hook.matcher, subject) {
                continue;
            }
            if !matches_keyword(hook, &payload) {
                continue;
            }
            let outcome = run_hook(hook, event, payload.clone()).await?;
            aggregate.allow &= outcome.allow;
            if !outcome.reason.is_empty() {
                aggregate.reason = outcome.reason;
            }
            if outcome.arguments.is_some() {
                aggregate.arguments = outcome.arguments;
            }
            if outcome.result.is_some() {
                aggregate.result = outcome.result;
            }
            if !outcome.additional_context.trim().is_empty() {
                if !aggregate.additional_context.is_empty() {
                    aggregate.additional_context.push_str("\n\n");
                }
                aggregate
                    .additional_context
                    .push_str(&outcome.additional_context);
            }
            if !aggregate.allow {
                break;
            }
        }
        Ok(aggregate)
    }
}

fn matches_keyword(hook: &HookConfig, payload: &Value) -> bool {
    if hook.keyword_match == KeywordMatch::Disabled {
        return true;
    }
    let keyword = hook.keyword.trim();
    if keyword.is_empty() {
        return false;
    }
    let Some(prompt) = payload.get("prompt").and_then(Value::as_str) else {
        return false;
    };
    match hook.keyword_match {
        KeywordMatch::Disabled => true,
        KeywordMatch::Exact => prompt
            .trim_matches(|character: char| !character.is_alphanumeric())
            .eq_ignore_ascii_case(keyword),
        KeywordMatch::Contains => contains_ignore_ascii_case(prompt, keyword),
    }
}

fn contains_ignore_ascii_case(value: &str, needle: &str) -> bool {
    value.to_lowercase().contains(&needle.to_lowercase())
}

async fn run_hook(hook: &HookConfig, event: HookEvent, payload: Value) -> Result<HookOutcome> {
    anyhow::ensure!(!hook.command.trim().is_empty(), "hook command is empty");
    let mut child = Command::new(&hook.command)
        .args(&hook.args)
        .envs(&hook.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start {} hook", event.key()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&serde_json::to_vec(&payload)?).await?;
        stdin.shutdown().await?;
    }
    let output = tokio::time::timeout(
        Duration::from_millis(hook.timeout_ms.clamp(100, 60_000)),
        child.wait_with_output(),
    )
    .await
    .context("hook timed out")??;
    anyhow::ensure!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    if output.stdout.is_empty() {
        return Ok(HookOutcome {
            allow: true,
            ..HookOutcome::default()
        });
    }
    serde_json::from_slice(&output.stdout).context("hook returned invalid JSON")
}

fn matches_subject(matcher: &str, subject: Option<&str>) -> bool {
    matcher == "*"
        || subject.is_none() && matcher.is_empty()
        || subject.is_some_and(|subject| matcher.eq_ignore_ascii_case(subject))
}

fn default_matcher() -> String {
    "*".into()
}

const fn default_timeout_ms() -> u64 {
    10_000
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_matches_every_tool() {
        assert!(matches_subject("*", Some("local_shell")));
        assert!(matches_subject("read_file", Some("READ_FILE")));
        assert!(!matches_subject("read_file", Some("write_file")));
    }

    fn keyword_hook(mode: KeywordMatch, keyword: &str) -> HookConfig {
        HookConfig {
            enabled: true,
            matcher: "*".into(),
            keyword: keyword.into(),
            keyword_match: mode,
            command: "/bin/true".into(),
            args: Vec::new(),
            timeout_ms: default_timeout_ms(),
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn exact_keyword_allows_surrounding_symbols_only() {
        let hook = keyword_hook(KeywordMatch::Exact, "开始备份");
        assert!(matches_keyword(
            &hook,
            &serde_json::json!({"prompt": "  【开始备份！】  "})
        ));
        assert!(!matches_keyword(
            &hook,
            &serde_json::json!({"prompt": "请开始备份！"})
        ));
    }

    #[test]
    fn contains_keyword_matches_inside_sentence() {
        let hook = keyword_hook(KeywordMatch::Contains, "Build APK");
        assert!(matches_keyword(
            &hook,
            &serde_json::json!({"prompt": "please build apk now"})
        ));
        assert!(!matches_keyword(
            &hook,
            &serde_json::json!({"prompt": "run tests"})
        ));
    }
}
