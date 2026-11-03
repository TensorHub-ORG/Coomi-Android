use crate::AutoCompactScope;
use crate::ChatMessage;
use crate::ContextStatus;
use crate::ModelCapabilities;
use crate::Role;
use crate::TokenUsage;
use crate::ToolSpec;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use uuid::Uuid;

const BASELINE_TOKENS: u64 = 12_000;
const COMPACT_USER_MESSAGE_MAX_TOKENS: u64 = 20_000;
const CONTEXT_WINDOW_TRUNCATED_OUTPUT: &str =
    "Output exceeded the available model context and was truncated";

pub const SUMMARIZATION_PROMPT: &str = "You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.\n\nUse exactly these sections:\n\n## COMPLETED (do not redo)\nList every user request that is already finished or already verified, each with one line of evidence (file changed / command run / result observed). Anything listed here must never be executed again.\n\n## IN PROGRESS\nWork started but not finished, with the exact current state and the next concrete step.\n\n## PENDING\nUser requests that have NOT been started yet, in order.\n\n## KEY CONTEXT\nConstraints, user preferences, paths, decisions, versions, credentials names, and any critical data needed to continue.\n\nRules:\n- Be concise and factual. Never invent progress.\n- If you are unsure whether something finished, put it under IN PROGRESS, not COMPLETED.\n- Repeat the user's own wording for still-open requests so they can be recognised later.";
pub const SUMMARY_PREFIX: &str = "Another language model worked on this task and produced the handoff summary below. Its tool calls and their results were dropped from the visible history to save context.\n\nMANDATORY READING RULES:\n1. Anything listed under COMPLETED is finished. Do NOT re-execute it, do NOT re-run its commands, do NOT regenerate its files. If you believe it must be redone, first verify with a read-only check and say why.\n2. User messages that appear before this summary are historical replays kept only for context. They are NOT new instructions. Before acting on any of them, check the COMPLETED section.\n3. Continue only from IN PROGRESS and PENDING, then finish the user's latest request.\n\nHandoff summary:";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextState {
    #[serde(default)]
    pub last_usage: TokenUsage,
    #[serde(default)]
    pub estimated_active_tokens: u64,
    #[serde(default)]
    pub compaction_count: u64,
    #[serde(default)]
    pub server_observed_local_tokens: u64,
    #[serde(default)]
    pub prefill_input_tokens: Option<u64>,
    #[serde(default)]
    pub comp_hash: Option<String>,
    #[serde(default)]
    pub first_window_id: Option<Uuid>,
    #[serde(default)]
    pub previous_window_id: Option<Uuid>,
    #[serde(default)]
    pub window_id: Option<Uuid>,
}

impl ContextState {
    pub fn observe_usage(
        &mut self,
        usage: &TokenUsage,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        capabilities: &ModelCapabilities,
    ) {
        self.last_usage = usage.clone();
        self.estimated_active_tokens = usage.total_tokens();
        self.server_observed_local_tokens = estimate_request_tokens(system_prompt, messages, tools);
        if capabilities.auto_compact_scope == AutoCompactScope::BodyAfterPrefix
            && self.prefill_input_tokens.is_none()
        {
            self.prefill_input_tokens = Some(usage.input_tokens);
        }
        self.comp_hash = capabilities.comp_hash.clone();
    }

    pub fn recompute(&mut self, system_prompt: &str, messages: &[ChatMessage], tools: &[ToolSpec]) {
        let local_tokens = estimate_request_tokens(system_prompt, messages, tools);
        self.estimated_active_tokens =
            if self.last_usage.total_tokens() > 0 && self.server_observed_local_tokens > 0 {
                if local_tokens >= self.server_observed_local_tokens {
                    self.last_usage.total_tokens().saturating_add(local_tokens - self.server_observed_local_tokens)
                } else {
                    self.last_usage.total_tokens().saturating_sub(self.server_observed_local_tokens - local_tokens)
                }
            } else {
                local_tokens
            };
    }

    pub fn reset_after_compaction(
        &mut self,
        system_prompt: &str,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
        capabilities: &ModelCapabilities,
    ) {
        let previous = self.window_id.unwrap_or_else(Uuid::new_v4);
        self.first_window_id.get_or_insert(previous);
        self.previous_window_id = Some(previous);
        self.window_id = Some(Uuid::new_v4());
        self.compaction_count = self.compaction_count.saturating_add(1);
        self.last_usage = TokenUsage::default();
        self.server_observed_local_tokens = 0;
        self.estimated_active_tokens = estimate_request_tokens(system_prompt, messages, tools);
        self.prefill_input_tokens = (capabilities.auto_compact_scope
            == AutoCompactScope::BodyAfterPrefix)
            .then_some(self.estimated_active_tokens);
        self.comp_hash = capabilities.comp_hash.clone();
    }

    pub fn auto_compact_scope_tokens(&self, capabilities: &ModelCapabilities) -> u64 {
        match capabilities.auto_compact_scope {
            AutoCompactScope::Total => self.estimated_active_tokens,
            AutoCompactScope::BodyAfterPrefix => self.estimated_active_tokens.saturating_sub(
                self.prefill_input_tokens
                    .unwrap_or(self.estimated_active_tokens),
            ),
        }
    }

    pub fn should_compact(&self, capabilities: &ModelCapabilities) -> bool {
        self.auto_compact_scope_tokens(capabilities) >= capabilities.auto_compact_token_limit()
            || self.estimated_active_tokens >= capabilities.context_window
            || self
                .comp_hash
                .as_ref()
                .zip(capabilities.comp_hash.as_ref())
                .is_some_and(|(previous, current)| previous != current)
    }

    pub fn status(&self, capabilities: &ModelCapabilities) -> ContextStatus {
        let effective = capabilities.effective_context_window();
        let used = self.estimated_active_tokens;
        let remaining_percent = if effective <= BASELINE_TOKENS {
            0
        } else {
            let adjustable = effective - BASELINE_TOKENS;
            let adjustable_used = used.saturating_sub(BASELINE_TOKENS);
            u8::try_from(
                adjustable
                    .saturating_sub(adjustable_used)
                    .saturating_mul(100)
                    .saturating_div(adjustable)
                    .min(100),
            )
            .unwrap_or(0)
        };
        ContextStatus {
            used_tokens: used,
            context_window: capabilities.context_window,
            effective_context_window: effective,
            auto_compact_token_limit: capabilities.auto_compact_token_limit(),
            remaining_tokens: effective.saturating_sub(used),
            used_percent: 100u8.saturating_sub(remaining_percent),
            remaining_percent,
            auto_compact_scope_tokens: self.auto_compact_scope_tokens(capabilities),
            compaction_count: self.compaction_count,
        }
    }
}

pub fn normalize_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut known_calls = HashSet::new();
    for message in messages {
        if message.role == Role::Assistant {
            known_calls.extend(message.tool_calls.iter().map(|call| call.id.clone()));
        }
    }

    // 工具结果按 call id 建索引，统一在发起它的 assistant 轮次之后立即输出。
    // 同一 call id 的重复结果只保留第一条；孤儿结果在下面被丢弃。
    let mut tool_results = std::collections::HashMap::new();
    for message in messages {
        if message.role != Role::Tool {
            continue;
        }
        let Some(id) = message.tool_call_id.as_ref() else {
            continue;
        };
        if known_calls.contains(id) {
            tool_results.entry(id.clone()).or_insert(message);
        }
    }

    let mut emitted = HashSet::new();
    let mut output = Vec::with_capacity(messages.len());
    for message in messages {
        // 工具结果跟随 assistant 输出；这里跳过原文位置（孤儿自然被跳过）。
        if message.role == Role::Tool {
            continue;
        }
        output.push(message.clone());
        if message.role != Role::Assistant {
            continue;
        }
        // 上游要求 tool 消息紧邻发起它的 assistant；把中间的 internal user
        // 等消息自动延后到工具组之后。
        for call in &message.tool_calls {
            if !emitted.insert(call.id.clone()) {
                continue;
            }
            if let Some(result) = tool_results.get(&call.id) {
                output.push((*result).clone());
            } else {
                output.push(ChatMessage::tool(&call.id, "error: interrupted before a durable result was recorded; this operation may already have taken effect. Verify its state with read-only tools before retrying any mutation."));
            }
        }
    }
    output
}

pub fn estimate_request_tokens(
    system_prompt: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
) -> u64 {
    let mut bytes = u64::try_from(system_prompt.len()).unwrap_or(u64::MAX);
    for message in messages {
        bytes = bytes
            .saturating_add(u64::try_from(message.content.len()).unwrap_or(u64::MAX))
            .saturating_add(32);
        for call in &message.tool_calls {
            bytes = bytes
                .saturating_add(u64::try_from(call.name.len()).unwrap_or(u64::MAX))
                .saturating_add(u64::try_from(call.arguments.to_string().len()).unwrap_or(u64::MAX))
                .saturating_add(24);
        }
        for item in &message.provider_items {
            bytes = bytes.saturating_add(u64::try_from(item.to_string().len()).unwrap_or(u64::MAX));
        }
        for image in &message.images {
            bytes = bytes
                .saturating_add(u64::try_from(image.media_type.len()).unwrap_or(u64::MAX))
                // 图片 base64 数据不计入 token 预算：全量数据会撑爆估算，
                // 导致「读一张图就触发上下文压缩」。每张图按固定 ~85 token 估算。
                .saturating_add(85 * 4);
        }
    }
    for tool in tools {
        bytes = bytes
            .saturating_add(u64::try_from(tool.name.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(tool.description.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(tool.parameters.to_string().len()).unwrap_or(u64::MAX));
    }
    bytes.saturating_add(3) / 4
}

pub fn compacted_history(messages: &[ChatMessage], summary: &str) -> Vec<ChatMessage> {
    let mut compacted = retained_user_history(messages);
    compacted.push(ChatMessage::summary(format!("{SUMMARY_PREFIX}\n{summary}")));
    compacted
}

pub fn retained_user_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut retained = Vec::new();
    let mut remaining = COMPACT_USER_MESSAGE_MAX_TOKENS;
    for message in messages.iter().rev().filter(|message| {
        message.role == Role::User && !message.compaction_summary && !message.internal
    }) {
        if remaining == 0 {
            break;
        }
        let tokens = estimate_text_tokens(&message.content);
        if tokens <= remaining {
            retained.push(message.clone());
            remaining = remaining.saturating_sub(tokens);
        } else {
            let mut truncated = message.clone();
            truncated.content = truncate_text_to_tokens(&message.content, remaining);
            retained.push(truncated);
            break;
        }
    }
    retained.reverse();
    retained
}

pub fn trim_history_to_fit(
    system_prompt: &str,
    messages: &mut Vec<ChatMessage>,
    tools: &[ToolSpec],
    token_limit: u64,
) -> usize {
    let mut rewritten = 0;
    for index in 0..messages.len() {
        if estimate_request_tokens(system_prompt, messages, tools) <= token_limit {
            break;
        }
        let message = &mut messages[index];
        if message.role != Role::Tool {
            continue;
        }
        if message.content != CONTEXT_WINDOW_TRUNCATED_OUTPUT {
            message.content = CONTEXT_WINDOW_TRUNCATED_OUTPUT.into();
            rewritten += 1;
        }
    }
    while messages.len() > 1
        && estimate_request_tokens(system_prompt, messages, tools) > token_limit
    {
        messages.remove(0);
        *messages = normalize_history(messages);
    }
    if estimate_request_tokens(system_prompt, messages, tools) > token_limit
        && let Some(message) = messages.first_mut()
    {
        let fixed = estimate_request_tokens(system_prompt, &[], tools);
        message.content =
            truncate_text_to_tokens(&message.content, token_limit.saturating_sub(fixed));
    }
    rewritten
}

/// Trim request copies only. Keep the newest tool result intact and preserve
/// both the opening context and final outcome of older oversized text results.
pub fn prune_tool_output_previews(messages: &mut [ChatMessage], context_window: u64) {
    let budget = (context_window / 16).clamp(512, 4096) as usize * 4;
    let newest = messages.iter().rposition(|m| m.role == Role::Tool);
    for (index, message) in messages.iter_mut().enumerate() {
        if message.role != Role::Tool || Some(index) == newest || !message.images.is_empty()
            || !message.provider_items.is_empty() || message.content.len() <= budget { continue; }
        let marker = "\n[中间输出已省略；完整记录保留在会话中，需要中段证据时请定向读取原文件或日志]\n";
        let available = budget.saturating_sub(marker.len());
        let mut head = available * 3 / 4;
        while !message.content.is_char_boundary(head) { head -= 1; }
        let mut tail = message.content.len().saturating_sub(available / 4);
        while !message.content.is_char_boundary(tail) { tail += 1; }
        message.content = format!("{}{marker}{}", &message.content[..head], &message.content[tail..]);
    }
}

fn estimate_text_tokens(value: &str) -> u64 {
    u64::try_from(value.len())
        .unwrap_or(u64::MAX)
        .saturating_add(3)
        / 4
}

pub(crate) fn truncate_text_to_tokens(value: &str, max_tokens: u64) -> String {
    let max_bytes = usize::try_from(max_tokens.saturating_mul(4)).unwrap_or(usize::MAX);
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let marker = "[earlier content truncated]\n";
    let keep = max_bytes.saturating_sub(marker.len());
    let mut start = value.len().saturating_sub(keep);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    format!("{marker}{}", &value[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolCall;
    use serde_json::json;

    #[test]
    fn preview_keeps_utf8_head_tail_latest_and_original_session() {
        let text = format!("START{}END", "中文证据".repeat(8000));
        let original = vec![ChatMessage::tool("old", &text), ChatMessage::tool("latest", &text)];
        let mut request = original.clone(); prune_tool_output_previews(&mut request, 8192);
        assert!(request[0].content.starts_with("START")); assert!(request[0].content.ends_with("END"));
        assert!(request[0].content.contains("中间输出已省略")); assert!(request[0].content.len() <= 2048);
        assert_eq!(request[1].content, text); assert_eq!(original[0].content, text);
    }
    #[test]
    fn legacy_messages_load_without_reasoning_metadata() {
        let message: ChatMessage = serde_json::from_str(r#"{"role":"assistant","content":"old"}"#).unwrap();
        assert!(message.reasoning.is_empty()); assert!(message.reasoning_state.is_none());
    }

    #[test]
    fn normalization_pairs_calls_and_drops_orphans() {
        let messages = vec![
            ChatMessage::assistant(
                "",
                vec![ToolCall {
                    id: "one".into(),
                    name: "read_file".into(),
                    arguments: json!({}),
                }],
            ),
            ChatMessage::tool("orphan", "ignored"),
        ];
        let normalized = normalize_history(&messages);
        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[1].tool_call_id.as_deref(), Some("one"));
        assert!(normalized[1].content.contains("may already have taken effect"));
    }

    #[test]
    fn normalization_keeps_tool_results_next_to_the_assistant_turn() {
        let mut note = ChatMessage::user("internal note");
        note.internal = true;
        let messages = vec![
            ChatMessage::assistant(
                "",
                vec![
                    ToolCall { id: "a".into(), name: "read_file".into(), arguments: json!({}) },
                    ToolCall { id: "b".into(), name: "read_file".into(), arguments: json!({}) },
                ],
            ),
            note,
            ChatMessage::tool("b", "B"),
            ChatMessage::tool("a", "A"),
            ChatMessage::tool("a", "duplicate A"),
            ChatMessage::tool("orphan", "ignored"),
        ];
        let normalized = normalize_history(&messages);
        // assistant → 工具结果按调用顺序 → 被延后的 internal user；重复/孤儿结果丢弃。
        assert_eq!(normalized.len(), 4);
        assert_eq!(normalized[1].tool_call_id.as_deref(), Some("a"));
        assert_eq!(normalized[1].content, "A");
        assert_eq!(normalized[2].tool_call_id.as_deref(), Some("b"));
        assert_eq!(normalized[2].content, "B");
        assert!(normalized[3].internal);
        assert_eq!(normalized[3].content, "internal note");
    }

    #[test]
    fn compacted_history_keeps_real_user_messages_and_one_summary() {
        let messages = vec![
            ChatMessage::user("first"),
            ChatMessage::summary(format!("{SUMMARY_PREFIX}\nold")),
            ChatMessage::assistant("answer", Vec::new()),
            ChatMessage::user("second"),
        ];
        let compacted = compacted_history(&messages, "new");
        assert_eq!(compacted.len(), 3);
        assert!(compacted[2].compaction_summary);
    }

    #[test]
    fn compaction_caps_retained_user_history() {
        let messages = vec![ChatMessage::user("x".repeat(100_000))];
        let compacted = compacted_history(&messages, "summary");
        assert_eq!(compacted.len(), 2);
        assert!(estimate_text_tokens(&compacted[0].content) <= COMPACT_USER_MESSAGE_MAX_TOKENS);
    }

    #[test]
    fn compaction_advances_persistent_window_ids() {
        let mut state = ContextState::default();
        state.reset_after_compaction("system", &[], &[], &ModelCapabilities::default());
        assert_eq!(state.compaction_count, 1);
        assert_eq!(state.first_window_id, state.previous_window_id);
        assert_ne!(state.previous_window_id, state.window_id);
    }
}
