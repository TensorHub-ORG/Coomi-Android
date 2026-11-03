use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelCapabilities {
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<u64>,
    #[serde(default)]
    pub auto_compact_scope: AutoCompactScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comp_hash: Option<String>,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u64,
    #[serde(default)]
    pub supports_remote_compaction: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_native_tools: bool,
    #[serde(default)]
    pub supports_web_search: bool,
    #[serde(default)]
    pub supports_parallel_tool_calls: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            context_window: default_context_window(),
            effective_context_window_percent: default_effective_context_window_percent(),
            auto_compact_token_limit: None,
            auto_compact_scope: AutoCompactScope::Total,
            comp_hash: None,
            max_output_tokens: default_max_output_tokens(),
            supports_remote_compaction: false,
            supports_vision: false,
            supports_native_tools: true,
            supports_web_search: false,
            supports_parallel_tool_calls: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoCompactScope {
    #[default]
    Total,
    BodyAfterPrefix,
}

impl ModelCapabilities {
    pub fn effective_context_window(&self) -> u64 {
        self.context_window
            .saturating_mul(u64::from(self.effective_context_window_percent))
            / 100
    }

    pub fn auto_compact_token_limit(&self) -> u64 {
        let derived = self.context_window.saturating_mul(9) / 10;
        self.auto_compact_token_limit
            .map_or(derived, |limit| limit.min(derived))
            .min(self.effective_context_window())
    }
}

const fn default_context_window() -> u64 {
    256_000
}

const fn default_effective_context_window_percent() -> u8 {
    95
}

const fn default_max_output_tokens() -> u64 {
    8_192
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ChatMessage {
    /// 稳定消息 id：供前端精确定位单条消息做编辑/删除/重新回答。
    /// 旧会话无 id 时惰性生成（serde default 兼容），会话加载时补齐。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub role: Role,
    pub content: String,
    /// Display-only reasoning; never replayed to a provider or summarizer.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_state: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compaction_summary: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub internal: bool,
    /// 数字生命体主动消息（气泡/开场问候）：由生命体队列直接写入，不来自模型。
    #[serde(default, skip_serializing_if = "is_false")]
    pub life_proactive: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_items: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageContent>,
}

const fn is_false(value: &bool) -> bool {
    !*value
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::plain(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::plain(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        let mut tool_calls = tool_calls;
        for call in &mut tool_calls {
            sanitize_json_encoded_data(&mut call.arguments);
        }
        Self {
            id: new_message_id(),
            role: Role::Assistant,
            content: sanitize_long_encoded_data(&content.into()),
            tool_calls,
            tool_call_id: None,
            reasoning: String::new(),
            reasoning_state: None,
            compaction_summary: false,
            internal: false,
            life_proactive: false,
            provider_items: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: new_message_id(),
            role: Role::Tool,
            content: sanitize_long_encoded_data(&content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            reasoning: String::new(),
            reasoning_state: None,
            compaction_summary: false,
            internal: false,
            life_proactive: false,
            provider_items: Vec::new(),
            images: Vec::new(),
        }
    }

    fn plain(role: Role, content: impl Into<String>) -> Self {
        Self {
            id: new_message_id(),
            role,
            content: sanitize_long_encoded_data(&content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning: String::new(),
            reasoning_state: None,
            compaction_summary: false,
            internal: false,
            life_proactive: false,
            provider_items: Vec::new(),
            images: Vec::new(),
        }
    }

    pub fn internal_user(content: impl Into<String>) -> Self {
        let mut message = Self::user(content);
        message.internal = true;
        message
    }

    pub fn summary(content: impl Into<String>) -> Self {
        let mut message = Self::user(content);
        message.compaction_summary = true;
        message
    }

    pub fn provider_item(item: Value) -> Self {
        let mut message = Self::assistant(String::new(), Vec::new());
        message.compaction_summary = true;
        let mut item = item;
        sanitize_json_encoded_data(&mut item);
        message.provider_items.push(item);
        message
    }
}

/// 生成一条稳定的消息 id（UUID v4 字符串）。
fn new_message_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Replace very long inline encodings before they enter model context or persistence.
/// Structured `ImageContent` is intentionally excluded and remains available to vision calls.
pub fn sanitize_long_encoded_data(input: &str) -> String {
    const MIN_ENCODED_CHARS: usize = 4_096;
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len().min(64 * 1024));
    let mut cursor = 0;
    let mut index = 0;
    while index < bytes.len() {
        if !is_base64_body_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_base64_body_byte(bytes[index]) {
            index += 1;
        }
        let mut padding = 0;
        while index < bytes.len() && bytes[index] == b'=' && padding < 2 {
            index += 1;
            padding += 1;
        }
        let encoded = &input[start..index];
        if encoded.len() < MIN_ENCODED_CHARS {
            continue;
        }
        let is_hex = encoded.len() % 2 == 0 && encoded.bytes().all(|byte| byte.is_ascii_hexdigit());
        let is_base64 =
            encoded.len() % 4 == 0 && encoded.bytes().filter(|byte| *byte == b'=').count() <= 2;
        if !is_hex && !is_base64 {
            continue;
        }
        output.push_str(&input[cursor..start]);
        let kind = if is_hex { "hex" } else { "base64" };
        output.push_str(&format!(
            "[encoded_data omitted type={kind} chars={} md5={:x}]",
            encoded.len(),
            md5::compute(encoded.as_bytes())
        ));
        cursor = index;
    }
    if cursor == 0 {
        return input.to_owned();
    }
    output.push_str(&input[cursor..]);
    output
}

fn is_base64_body_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
}

pub fn sanitize_json_encoded_data(value: &mut Value) {
    match value {
        Value::String(text) => *text = sanitize_long_encoded_data(text),
        Value::Array(values) => values.iter_mut().for_each(sanitize_json_encoded_data),
        Value::Object(values) => values.values_mut().for_each(sanitize_json_encoded_data),
        _ => {}
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    pub fn resource_key(&self) -> Option<String> {
        [
            "path",
            "file",
            "directory",
            "cwd",
            "session_id",
            "id",
            "name",
        ]
        .iter()
        .find_map(|key| self.arguments.get(*key).and_then(Value::as_str))
        .map(|value| value.replace('\\', "/").to_ascii_lowercase())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InvalidToolCall {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolConcurrency {
    ReadOnly,
    Mutating,
    Destructive,
    Interactive,
}

impl ToolSpec {
    /// Conservative scheduling metadata for built-ins. Unknown/MCP tools stay serial
    /// until they explicitly gain a trusted classification.
    pub fn concurrency(&self) -> ToolConcurrency {
        match self.name.as_str() {
            "read_file" | "list_dir" | "search" | "grep_files" | "web_search" | "fetch"
            | "view_image" | "show_image" | "list_skills" | "read_skill" | "memory_list"
            | "memory_read" | "memory_search" | "list_mcp" | "get_loop" | "wait_agent" => {
                ToolConcurrency::ReadOnly
            }
            "uninstall_mcp" | "uninstall_skill" | "memory_delete" | "close_agent" => {
                ToolConcurrency::Destructive
            }
            "request_user_input" | "request_file_import" | "request_file_export" => {
                ToolConcurrency::Interactive
            }
            _ => ToolConcurrency::Mutating,
        }
    }

    pub fn background_capable(&self) -> bool {
        matches!(self.name.as_str(), "shell" | "local_shell" | "wait_agent")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageContent {
    pub media_type: String,
    pub data: String,
}

impl ImageContent {
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub reasoning_effort: Option<String>,
    /// 上游可用于路由与提示词缓存的稳定 Coomi 会话标识。
    pub session_id: Option<String>,
    /// DeepSeek 账号：是否开启联网搜索（网页版开关）。
    pub search_enabled: bool,
    /// DeepSeek 账号：是否开启深度思考（网页版开关）。
    pub thinking_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct CompactionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub system_prompt: String,
    pub tools: Vec<ToolSpec>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CompactionResponse {
    pub messages: Vec<ChatMessage>,
    pub usage: TokenUsage,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub cache_observed_input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_data_available: bool,
}

impl TokenUsage {
    pub fn add(&mut self, other: &Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.cache_observed_input_tokens = self
            .cache_observed_input_tokens
            .saturating_add(other.cache_observed_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_data_available |= other.cache_data_available;
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn saturating_sub(&self, previous: &Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .saturating_sub(previous.cached_input_tokens),
            cache_observed_input_tokens: self
                .cache_observed_input_tokens
                .saturating_sub(previous.cache_observed_input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
            cache_data_available: self.cache_data_available,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ModelResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// Tool calls that were rejected before execution because their arguments
    /// could not be normalized to a JSON object.
    pub invalid_tool_calls: Vec<InvalidToolCall>,
    pub usage: TokenUsage,
    pub streamed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    Http,
    Timeout,
    Connect,
    Dns,
    Tls,
    Proxy,
    Redirect,
    RequestBuild,
    RequestBody,
    LocalIo,
    Request,
    Stream,
    Decode,
}

#[derive(Debug)]
pub struct ProviderRequestError {
    pub phase: &'static str,
    pub kind: ProviderErrorKind,
    pub status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub request_id: Option<String>,
    pub retryable: bool,
    pub detail: String,
}

impl std::fmt::Display for ProviderRequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "provider error [phase={} kind={:?} retryable={}",
            self.phase, self.kind, self.retryable
        )?;
        if let Some(status) = self.status {
            write!(formatter, " status={status}")?;
        }
        if let Some(retry_after_ms) = self.retry_after_ms {
            write!(formatter, " retry_after_ms={retry_after_ms}")?;
        }
        if let Some(request_id) = &self.request_id {
            write!(formatter, " request_id={request_id}")?;
        }
        write!(formatter, "]: {}", self.detail)
    }
}

impl std::error::Error for ProviderRequestError {}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub plan: Option<PlanState>,
    pub loop_state: Option<LoopState>,
    pub additional_context: Option<String>,
    pub images: Vec<ImageContent>,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            plan: None,
            loop_state: None,
            additional_context: None,
            images: Vec::new(),
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            success: false,
            output: output.into(),
            plan: None,
            loop_state: None,
            additional_context: None,
            images: Vec::new(),
        }
    }

    pub fn with_plan(mut self, plan: PlanState) -> Self {
        self.plan = Some(plan);
        self
    }

    pub fn with_loop(mut self, loop_state: LoopState) -> Self {
        self.loop_state = Some(loop_state);
        self
    }

    pub fn with_additional_context(mut self, context: impl Into<String>) -> Self {
        self.additional_context = Some(context.into());
        self
    }

    pub fn with_image(mut self, media_type: impl Into<String>, data: impl Into<String>) -> Self {
        self.images.push(ImageContent {
            media_type: media_type.into(),
            data: data.into(),
        });
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanState {
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

impl PlanState {
    pub fn validate(&self) -> Result<(), String> {
        if self.steps.is_empty() {
            return Err("plan must contain at least one step".into());
        }
        if self
            .steps
            .iter()
            .filter(|step| step.status == PlanStepStatus::InProgress)
            .count()
            > 1
        {
            return Err("at most one plan step may be in progress".into());
        }
        if self.steps.iter().any(|step| step.step.trim().is_empty()) {
            return Err("plan steps must not be empty".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LoopState {
    pub objective: String,
    pub status: LoopStatus,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub time_used_seconds: u64,
    #[serde(default)]
    pub blocked_streak: u8,
    #[serde(default)]
    pub turns_completed: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserInputRequest {
    pub questions: Vec<UserInputQuestion>,
    pub auto_resolution_ms: Option<u64>,
}

pub type UserInputResponse = BTreeMap<String, String>;

// ─────────────────────────────────────────────────────────────────────────────
// Workflow 可编排多步骤执行（作为 Coomi 拓展能力，落点在 .coomi/workflows/<id>/）
// ─────────────────────────────────────────────────────────────────────────────

/// 单个步骤可绑定的执行能力（混合声明式）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StepAction {
    /// 一次独立的模型调用，使用本步骤自己的 prompt（可配置是否隔离上下文）。
    Model {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// 是否使用隔离的子会话上下文；缺省跟随 workflow 的 model_isolation 设置。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        isolate: Option<bool>,
    },
    /// 直接调用一个具体工具（不经过模型推理）。
    Tool {
        tool: String,
        #[serde(default)]
        arguments: serde_json::Value,
    },
    /// 在工作目录执行一条 shell 命令。
    Script {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_s: Option<u64>,
    },
    /// 嵌套执行一个已注册的子 workflow（组合复用）。
    SubWorkflow {
        #[serde(default)]
        workflow: String,
    },
    /// Pause execution until the user approves this checkpoint.
    Approval { prompt: String },
}

/// 步骤的完成状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStepState {
    Pending,
    Waiting,
    Running,
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

impl Default for WorkflowStepState {
    fn default() -> Self {
        Self::Pending
    }
}

/// 一个可编排的步骤。`depends_on` 是其前置步骤 id，构成 DAG 边。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowStep {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub action: StepAction,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub retry: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
    #[serde(default)]
    pub state: WorkflowStepState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl WorkflowStep {
    pub fn new(id: impl Into<String>, name: impl Into<String>, action: StepAction) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: String::new(),
            action,
            depends_on: Vec::new(),
            retry: 0,
            timeout_s: None,
            state: WorkflowStepState::Pending,
            result: None,
            attempts: 0,
            duration_ms: None,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn depends_on(mut self, ids: &[&str]) -> Self {
        self.depends_on = ids.iter().map(|s| (*s).to_string()).collect();
        self
    }

    pub fn with_retry(mut self, retry: u32) -> Self {
        self.retry = retry;
        self
    }
}

/// workflow 整体状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl Default for WorkflowStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// workflow 的来源（用于区分内置/用户/模型生成）。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowOrigin {
    Model,
    User,
    Imported,
    Builtin,
}

impl Default for WorkflowOrigin {
    fn default() -> Self {
        Self::User
    }
}

impl WorkflowOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::User => "user",
            Self::Imported => "imported",
            Self::Builtin => "builtin",
        }
    }
}

/// 工作流的定时调度配置（cron 表达式，引擎 scheduler 每分钟检查匹配）。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowSchedule {
    /// 是否启用定时触发；关闭后仅支持手动运行。
    #[serde(default)]
    pub enabled: bool,
    /// cron 表达式（如 `0 8 * * *`）；None 或空串表示未配置定时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
}

impl WorkflowSchedule {
    pub fn is_active(&self) -> bool {
        self.enabled
            && self
                .cron
                .as_deref()
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false)
    }
}

fn default_workflow_permission() -> String {
    "ask".into()
}

pub(crate) fn valid_workflow_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// 一个可编排工作流的完整定义（定义文件落点为 .coomi/workflows/<id>/workflow.json）。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    /// Host or mapped guest working directory; absent uses the engine workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<std::path::PathBuf>,
    #[serde(default = "default_workflow_permission")]
    pub permission: String,
    #[serde(default)]
    pub description: String,
    pub steps: Vec<WorkflowStep>,
    #[serde(default)]
    pub status: WorkflowStatus,
    #[serde(default)]
    pub origin: WorkflowOrigin,
    /// Model 类型步骤默认是否隔离上下文。
    #[serde(default)]
    pub model_isolation: bool,
    /// 定时调度配置（P1：scheduler 定时触发；缺省关闭）。
    #[serde(default)]
    pub schedule: WorkflowSchedule,
    /// workflow 定义运行时的临时变量（步骤之间传递数据的通道）。
    #[serde(default)]
    pub variables: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl WorkflowState {
    pub fn new(id: impl Into<String>, name: impl Into<String>, steps: Vec<WorkflowStep>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            cwd: None,
            permission: default_workflow_permission(),
            description: String::new(),
            steps,
            status: WorkflowStatus::Pending,
            origin: WorkflowOrigin::User,
            model_isolation: false,
            variables: BTreeMap::new(),
            schedule: WorkflowSchedule::default(),
            created_at: None,
            updated_at: None,
        }
    }

    /// 校验依赖图：无自环、无未知依赖、无重复 id、每个依赖最终可执行。
    /// 返回给定步骤的拓扑可执行顺序（DAG，允许多个无依赖的根并行）。
    pub fn validate(&self) -> Result<(), String> {
        if !valid_workflow_id(&self.id) {
            return Err("workflow id must contain only ASCII letters, digits, - or _".into());
        }
        if !matches!(self.permission.as_str(), "ask" | "auto" | "full") {
            return Err("workflow permission must be ask, auto or full".into());
        }
        if self.steps.is_empty() {
            return Err("workflow must contain at least one step".into());
        }
        let mut seen = std::collections::HashSet::new();
        for step in &self.steps {
            if !valid_workflow_id(&step.id) {
                return Err("step id must contain only ASCII letters, digits, - or _".into());
            }
            if step.retry > 10
                || step
                    .timeout_s
                    .is_some_and(|seconds| seconds == 0 || seconds > 3600)
            {
                return Err(format!(
                    "step `{}` requires retry <= 10 and timeout_s between 1 and 3600",
                    step.id
                ));
            }
            if !seen.insert(step.id.as_str()) {
                return Err(format!("duplicate step id `{}`", step.id));
            }
        }
        for step in &self.steps {
            for dep in &step.depends_on {
                if dep == &step.id {
                    return Err(format!("step `{}` depends on itself", step.id));
                }
                if !seen.contains(dep.as_str()) {
                    return Err(format!(
                        "step `{}` depends on unknown step `{}`",
                        step.id, dep
                    ));
                }
            }
        }
        self.topological_order()?;
        Ok(())
    }

    /// 返回一个拓扑顺序。若存在循环或未知依赖则报错。
    pub fn topological_order(&self) -> Result<Vec<String>, String> {
        let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
        let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for step in &self.steps {
            indegree.entry(step.id.clone()).or_insert(0);
            dependents.entry(step.id.clone()).or_default();
        }
        for step in &self.steps {
            for dep in &step.depends_on {
                // indegree 是「步骤自身有多少条入边」= depends_on 数量。
                if let Some(deg) = indegree.get_mut(&step.id) {
                    *deg += 1;
                }
                // dependents 是「被依赖者的后继」= 依赖它的步骤。
                if let Some(children) = dependents.get_mut(dep) {
                    children.push(step.id.clone());
                }
            }
        }
        // Kahn's algorithm
        let mut queue: Vec<String> = indegree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(id, _)| id.clone())
            .collect();
        queue.sort();
        let mut order: Vec<String> = Vec::new();
        let mut temp = queue;
        while let Some(node) = temp.first().cloned() {
            temp.remove(0);
            order.push(node.clone());
            if let Some(children) = dependents.get(&node) {
                for child in children {
                    if let Some(deg) = indegree.get_mut(child) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            temp.push(child.clone());
                        }
                    }
                }
            }
            temp.sort();
            temp.dedup();
        }
        if order.len() != self.steps.len() {
            return Err("workflow dependency graph contains a cycle".into());
        }
        Ok(order)
    }

    /// 返回当前"可执行"的步骤 id：所有依赖已 succeeded 自身仍 Pending。
    pub fn ready_steps(&self) -> Vec<String> {
        let state_of = |id: &str| self.steps.iter().find(|s| s.id == id).map(|s| s.state);
        let mut ready = Vec::new();
        for step in &self.steps {
            if step.state != WorkflowStepState::Pending {
                continue;
            }
            let all_deps_ok = step
                .depends_on
                .iter()
                .all(|dep| state_of(dep) == Some(WorkflowStepState::Succeeded));
            if all_deps_ok {
                ready.push(step.id.clone());
            }
        }
        ready
    }

    /// A new run never inherits execution state from a previous run or old definition.
    pub fn reset_for_run(&mut self) {
        self.status = WorkflowStatus::Pending;
        for step in &mut self.steps {
            self.variables.remove(&step.id);
            step.state = WorkflowStepState::Pending;
            step.result = None;
            step.attempts = 0;
            step.duration_ms = None;
        }
    }

    /// 所有步骤是否都到达终态。
    pub fn is_terminal(&self) -> bool {
        self.steps.iter().all(|s| {
            matches!(
                s.state,
                WorkflowStepState::Succeeded
                    | WorkflowStepState::Failed
                    | WorkflowStepState::Skipped
                    | WorkflowStepState::Cancelled
            )
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileTransferRequest {
    pub request_id: String,
    pub operation: String,
    pub path: Option<String>,
    pub suggested_name: Option<String>,
    pub multiple: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    ModelStarted {
        provider: String,
        model: String,
        round: usize,
    },
    ConnectionRetry {
        attempt: u8,
        max_attempts: u8,
        delay_ms: u64,
        message: String,
    },
    /// Discard partial stream output before retrying the same model request.
    StreamReset,
    Text(String),
    TextDelta(String),
    ReasoningDelta(String),
    ReasoningCompleted,
    PersistenceFailed(String),
    ContextUpdated(ContextStatus),
    /// Usage reported by one completed model request. This is emitted after
    /// every tool-loop model call so observers can refresh live statistics.
    ModelUsage {
        total: TokenUsage,
        request: TokenUsage,
    },
    CompactionStarted {
        automatic: bool,
    },
    CompactionCompleted {
        automatic: bool,
        before_tokens: u64,
        after_tokens: u64,
        summary_id: String,
        summary: String,
    },
    PlanUpdated(PlanState),
    LoopUpdated(LoopState),
    QueuedInputAccepted(Vec<String>),
    ToolStarted(ToolCall),
    ToolFinished {
        call: ToolCall,
        result: ToolResult,
    },
    TurnCompleted {
        total: TokenUsage,
        turn: TokenUsage,
    },
}

pub trait AgentObserver: Send + Sync {
    fn on_event(&self, event: &AgentEvent);
}

#[async_trait]
pub trait TurnControl: Send + Sync {
    /// Wait at a resumable boundary. Implementations must not report a running
    /// model request or tool subprocess as paused before this method is reached.
    async fn safe_point(&self) -> Result<()>;
}

pub struct NoopObserver;

impl AgentObserver for NoopObserver {
    fn on_event(&self, _event: &AgentEvent) {}
}

#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn approve(&self, call: &ToolCall, reason: &str) -> bool;

    async fn request_user_input(&self, _request: &UserInputRequest) -> Option<UserInputResponse> {
        None
    }

    async fn request_file_transfer(&self, _request: &FileTransferRequest) -> Option<Vec<String>> {
        None
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn model(&self) -> &str;
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse>;

    async fn complete_stream(
        &self,
        request: ModelRequest,
        _observer: &dyn ModelStreamObserver,
    ) -> Result<ModelResponse> {
        self.complete(request).await
    }

    async fn compact(&self, _request: CompactionRequest) -> Result<Option<CompactionResponse>> {
        Ok(None)
    }
}

pub trait ModelStreamObserver: Send + Sync {
    fn on_text_delta(&self, delta: &str);
    fn on_reasoning_delta(&self, delta: &str);
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextStatus {
    pub used_tokens: u64,
    pub context_window: u64,
    pub effective_context_window: u64,
    pub auto_compact_token_limit: u64,
    pub remaining_tokens: u64,
    pub used_percent: u8,
    pub remaining_percent: u8,
    pub auto_compact_scope_tokens: u64,
    pub compaction_count: u64,
}

#[async_trait]
pub trait ToolRuntime: Send + Sync {
    fn specs(&self) -> Vec<ToolSpec>;
    /// Refresh task context before each model/tool round, including queued input.
    fn update_history(&self, _messages: &[ChatMessage]) {}
    async fn call(&self, call: &ToolCall, approval: &dyn ApprovalHandler) -> ToolResult;

    async fn lifecycle(&self, _event: &str, _payload: Value) -> Result<Option<String>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::sanitize_long_encoded_data;

    #[test]
    fn removes_one_megabyte_base64_without_retaining_payload() {
        let payload = "QUJD".repeat(256 * 1024);
        let sanitized = sanitize_long_encoded_data(&format!("data:image/png;base64,{payload}"));
        assert!(sanitized.contains("encoded_data omitted type=base64"));
        assert!(sanitized.contains("chars=1048576"));
        assert!(!sanitized.contains(&payload[..4096]));
        assert!(sanitized.len() < 256);
    }

    #[test]
    fn keeps_hashes_and_regular_jwts() {
        let hash = "0123456789abcdef".repeat(4);
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature";
        let input = format!("hash={hash} jwt={jwt}");
        assert_eq!(sanitize_long_encoded_data(&input), input);
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::{StepAction, WorkflowState, WorkflowStep, WorkflowStepState};

    fn model_step(id: &str, name: &str, deps: &[&str]) -> WorkflowStep {
        WorkflowStep::new(
            id,
            name,
            StepAction::Model {
                prompt: "hi".into(),
                model: None,
                isolate: None,
            },
        )
        .depends_on(deps)
    }

    fn linear_workflow() -> WorkflowState {
        WorkflowState::new(
            "wf-1",
            "linear",
            vec![
                model_step("a", "A", &[]),
                model_step("b", "B", &["a"]),
                model_step("c", "C", &["b"]),
            ],
        )
    }

    #[test]
    fn topological_order_is_respected() {
        let wf = linear_workflow();
        wf.validate().expect("valid");
        assert_eq!(wf.topological_order().unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn parallel_branches_are_detected() {
        let wf = WorkflowState::new(
            "wf-2",
            "parallel",
            vec![
                model_step("root", "root", &[]),
                model_step("left", "L", &["root"]),
                model_step("right", "R", &["root"]),
                model_step("join", "join", &["left", "right"]),
            ],
        );
        wf.validate().expect("valid");
        // root 先，然后 left/right 可并行（顺序不定但都在 join 前）
        let order = wf.topological_order().unwrap();
        assert_eq!(order[0], "root");
        assert_eq!(order[3], "join");
        assert!(order.contains(&"left".to_string()));
        assert!(order.contains(&"right".to_string()));
    }

    #[test]
    fn cycle_is_rejected() {
        let wf = WorkflowState::new(
            "wf-3",
            "cycle",
            vec![model_step("a", "A", &["b"]), model_step("b", "B", &["a"])],
        );
        assert!(wf.validate().is_err());
    }

    #[test]
    fn unknown_dependency_is_rejected() {
        let wf = WorkflowState::new("wf-4", "bad-dep", vec![model_step("a", "A", &["missing"])]);
        assert!(wf.validate().is_err());
    }

    #[test]
    fn self_dependency_is_rejected() {
        let wf = WorkflowState::new("wf-5", "self-dep", vec![model_step("a", "A", &["a"])]);
        assert!(wf.validate().is_err());
    }

    #[test]
    fn duplicate_step_id_is_rejected() {
        let wf = WorkflowState::new(
            "wf-6",
            "dup",
            vec![model_step("a", "A", &[]), model_step("a", "A2", &[])],
        );
        assert!(wf.validate().is_err());
    }

    #[test]
    fn empty_steps_are_rejected() {
        let wf = WorkflowState::new("wf-7", "empty", vec![]);
        assert!(wf.validate().is_err());
    }

    #[test]
    fn ready_steps_only_include_unblocked_pending() {
        let mut wf = linear_workflow();
        // 初始只有根 a 就绪
        assert_eq!(wf.ready_steps(), vec!["a"]);
        // 标记 a 成功，b 就绪
        wf.steps[0].state = WorkflowStepState::Succeeded;
        assert_eq!(wf.ready_steps(), vec!["b"]);
        // a 仍 pending 时 b 不因 c 就绪
        wf.steps[1].state = WorkflowStepState::Pending;
        wf.steps[0].state = WorkflowStepState::Pending;
        assert_eq!(wf.ready_steps(), vec!["a"]);
    }

    #[test]
    fn is_terminal_when_all_finished() {
        let mut wf = linear_workflow();
        for step in &mut wf.steps {
            step.state = WorkflowStepState::Succeeded;
        }
        assert!(wf.is_terminal());
        wf.steps[0].state = WorkflowStepState::Running;
        assert!(!wf.is_terminal());
    }

    #[test]
    fn validate_accepts_diamond() {
        let wf = WorkflowState::new(
            "wf-8",
            "diamond",
            vec![
                model_step("a", "A", &[]),
                model_step("b", "B", &["a"]),
                model_step("c", "C", &["a"]),
                model_step("d", "D", &["b", "c"]),
            ],
        );
        wf.validate().expect("diamond valid");
    }

    #[test]
    fn model_step_defaults_to_pending() {
        let step = model_step("s", "S", &[]);
        assert_eq!(step.state, WorkflowStepState::Pending);
        assert_eq!(step.attempts, 0);
        assert!(step.result.is_none());
    }
}
