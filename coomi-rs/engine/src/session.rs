use crate::ChatMessage;
use crate::ContextState;
use crate::LoopState;
use crate::PlanState;
use crate::TokenUsage;
use crate::types::sanitize_json_encoded_data;
use crate::types::sanitize_long_encoded_data;
use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::cmp::Reverse;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use uuid::Uuid;

static SESSION_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
pub(crate) const TASK_COMPLETION_CHECK: &str = "Check whether the current task is complete. If it is not, continue until it is; if it is complete, briefly state the conclusion.";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Agent,
    Life,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Session {
    /// 辅助会话：父会话 id（1.4.8 辅助对话体系）。
    #[serde(default)]
    pub parent_session_id: Option<Uuid>,
    pub id: Uuid,
    pub provider_id: String,
    pub model: String,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<ChatMessage>,
    pub usage: TokenUsage,
    /// Versioned conversation mode. Missing values from older session files
    /// deserialize as Agent so long-term Life memory never enters code sessions.
    #[serde(default)]
    pub mode: SessionMode,
    /// 会话标题：首条用户消息的本地推导，供会话列表/检索使用。
    #[serde(default)]
    pub title: String,
    /// 用户手动修改过标题；检查点保存不能用运行中内存里的旧标题覆盖它。
    #[serde(default)]
    pub title_manually_set: bool,
    /// 会话置顶状态，必须随会话文件持久化，不能只依赖 WebView localStorage。
    #[serde(default)]
    pub pinned: bool,
    /// 会话一句话摘要：本地规则推导（首条 user + 末尾 assistant），供检索匹配。
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub context: ContextState,
    #[serde(default)]
    pub plan: Option<PlanState>,
    #[serde(default)]
    pub loop_state: Option<LoopState>,
    #[serde(default)]
    pub hooks_started: bool,
    /// User turn whose automatic completion check was already started.
    /// Written before requesting the provider, so reload/recovery cannot duplicate it.
    #[serde(default)]
    pub completion_checked_turn: Option<String>,
    /// 压缩前的完整历史存档：压缩发生时备份，压缩后仍可从磁盘恢复完整展示。
    #[serde(default)]
    pub archive: Vec<ChatMessage>,
}

impl Session {
    pub fn new(provider_id: impl Into<String>, model: impl Into<String>, cwd: PathBuf) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            provider_id: provider_id.into(),
            model: model.into(),
            cwd,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            usage: TokenUsage::default(),
            mode: SessionMode::Agent,
            title: String::new(),
            title_manually_set: false,
            parent_session_id: None,
            pinned: false,
            summary: String::new(),
            context: ContextState::default(),
            plan: None,
            loop_state: None,
            hooks_started: false,
            completion_checked_turn: None,
            archive: Vec::new(),
        }
    }

    pub fn switch_model(&mut self, provider_id: impl Into<String>, model: impl Into<String>) {
        self.provider_id = provider_id.into();
        self.model = model.into();
        self.touch();
    }

    pub fn clear_data(&mut self) {
        self.messages.clear();
        self.archive.clear();
        self.usage = TokenUsage::default();
        self.context = ContextState::default();
        self.plan = None;
        self.loop_state = None;
        self.hooks_started = false;
        self.completion_checked_turn = None;
        self.summary.clear();
        self.touch();
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    /// 按 id 定位消息在 `messages` 中的下标。找不到返回 None。
    pub fn find_message(&self, id: &str) -> Option<usize> {
        self.messages.iter().position(|message| message.id == id)
    }

    /// 编辑单条消息正文（仅改文本；工具调用/角色不动）。
    /// 找不到该 id 时报错。
    pub fn edit_message(&mut self, id: &str, new_content: &str) -> Result<()> {
        let index = self
            .find_message(id)
            .ok_or_else(|| anyhow::anyhow!("message {id} not found in session"))?;
        self.messages[index].content = new_content.to_owned();
        self.touch();
        Ok(())
    }

    /// 截断到指定消息 id 及其之后：保留 `[0, index)`，删除 `[index, len)`。
    /// 用于「以该提问为起点重新回答」—— 截断掉该提问及其后的回复/工具结果。
    pub fn truncate_from(&mut self, id: &str) -> Result<usize> {
        let index = self
            .find_message(id)
            .ok_or_else(|| anyhow::anyhow!("message {id} not found in session"))?;
        let removed = self.messages.len() - index;
        self.messages.truncate(index);
        self.touch();
        Ok(removed)
    }

    /// 删除指定消息（含其后的工具结果消息），返回删除的消息数。
    /// 若该消息指向一条 assistant，会连同它关联的 tool 结果一起删除。
    pub fn delete_message(&mut self, id: &str) -> Result<usize> {
        let index = self
            .find_message(id)
            .ok_or_else(|| anyhow::anyhow!("message {id} not found in session"))?;
        let role = self.messages[index].role;
        let mut removed = 1;
        // 删除 assistant 时，把紧随其后的 tool 结果消息也一并移除（它们属于该回复）。
        if role == crate::Role::Assistant {
            while index + removed < self.messages.len()
                && self.messages[index + removed].role == crate::Role::Tool
            {
                removed += 1;
            }
        }
        self.messages.drain(index..index + removed);
        self.touch();
        Ok(removed)
    }
}

impl SessionStore {
    /// 刷新会话的「最后执行时间」并落盘。
    /// 列表排序以此为准：无论 agent 执行完成、被用户取消还是意外中断，
    /// 都要记录最后一次执行的时间（cancel 等路径不会走 run_turn 的 save）。
    pub fn touch_updated_at(&self, id: Uuid) -> Result<()> {
        let mut session = self.load(id)?;
        session.touch();
        self.save(&session)
    }
    pub fn clear_data(&self, id: Uuid) -> Result<Session> {
        let mut session = self.load(id)?;
        session.clear_data();
        self.save(&session)?;
        Ok(session)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub id: Uuid,
    pub provider_id: String,
    pub model: String,
    pub cwd: PathBuf,
    pub updated_at: DateTime<Utc>,
    /// 首条用户消息的短预览（向后兼容）。
    pub preview: String,
    /// 会话标题：持久化的 Session.title，缺省时惰性推导。
    pub title: String,
    pub title_manually_set: bool,
    pub pinned: bool,
    /// 会话摘要：持久化的 Session.summary，缺省时惰性推导。
    pub summary: String,
}

pub struct SessionStore {
    directory: PathBuf,
}

impl SessionStore {
    pub fn new(coomi_home: impl AsRef<Path>) -> Self {
        Self {
            directory: coomi_home.as_ref().join("sessions"),
        }
    }

    pub fn save(&self, session: &Session) -> Result<()> {
        let _guard = SESSION_WRITE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        fs::create_dir_all(&self.directory).with_context(|| {
            format!(
                "failed to create session directory {}",
                self.directory.display()
            )
        })?;
        let path = self.path(session.id);
        let mut persisted = session.clone();
        for message in &mut persisted.messages {
            message.content = sanitize_long_encoded_data(&message.content);
            for item in &mut message.provider_items {
                sanitize_json_encoded_data(item);
            }
            for call in &mut message.tool_calls {
                sanitize_json_encoded_data(&mut call.arguments);
            }
        }
        // 用户可能在 agent 运行期间改名/置顶。运行中的 Session 是较早快照，
        // 每次 checkpoint 都必须保留磁盘上更新后的用户元数据。
        if let Ok(bytes) = fs::read(&path)
            && let Ok(existing) = serde_json::from_slice::<Session>(&bytes)
        {
            if existing.title_manually_set {
                persisted.title = existing.title;
                persisted.title_manually_set = true;
            }
            persisted.pinned = existing.pinned;
        }
        let bytes = serde_json::to_vec_pretty(&persisted)?;
        // 原子写：先写临时文件再 rename，避免崩溃/断电留下截断的 JSON，
        // 防止会话记录“莫名消失”（损坏文件此前会被 load 失败后静默丢弃）。
        let tmp = self.directory.join(format!("{}.json.tmp", session.id));
        {
            use std::io::Write;
            let mut file = fs::File::create(&tmp)
                .with_context(|| format!("failed to write session {}", tmp.display()))?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path).with_context(|| {
            format!(
                "failed to commit session {} ({} -> {})",
                session.id,
                tmp.display(),
                path.display()
            )
        })?;
        #[cfg(unix)]
        fs::File::open(&self.directory)?.sync_all()?;
        Ok(())
    }

    pub fn load(&self, id: Uuid) -> Result<Session> {
        let path = self.path(id);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read session {}", path.display()))?;
        let mut session: Session = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid session file {}", path.display()))?;
        // Older releases persisted automatic checks as ordinary user messages.
        // Keep model context and assistant results while hiding the injected prompts.
        for message in session
            .messages
            .iter_mut()
            .chain(session.archive.iter_mut())
        {
            if message.role == crate::Role::User && message.content == TASK_COMPLETION_CHECK {
                message.internal = true;
            }
        }
        Ok(session)
    }

    pub fn delete(&self, id: Uuid) -> Result<bool> {
        let path = self.path(id);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(&path)
            .with_context(|| format!("failed to delete session {}", path.display()))?;
        Ok(true)
    }

    pub fn update_metadata(
        &self,
        id: Uuid,
        title: Option<&str>,
        pinned: Option<bool>,
    ) -> Result<Session> {
        let _guard = SESSION_WRITE_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.path(id);
        let bytes = fs::read(&path)
            .with_context(|| format!("failed to read session {}", path.display()))?;
        let mut session: Session = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid session file {}", path.display()))?;
        if let Some(title) = title {
            session.title = title.to_owned();
            session.title_manually_set = true;
        }
        if let Some(pinned) = pinned {
            session.pinned = pinned;
        }
        let tmp = self.directory.join(format!("{}.json.tmp", session.id));
        fs::write(&tmp, serde_json::to_vec_pretty(&session)?)
            .with_context(|| format!("failed to write session {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("failed to commit session metadata {}", session.id))?;
        Ok(session)
    }

    /// Whether a session file exists on disk for this id.
    pub fn contains(&self, id: Uuid) -> bool {
        self.path(id).exists()
    }

    pub fn latest(&self, cwd: Option<&Path>) -> Result<Option<Session>> {
        let summaries = self.list(cwd)?;
        summaries
            .first()
            .map(|summary| self.load(summary.id))
            .transpose()
    }

    pub fn list(&self, cwd: Option<&Path>) -> Result<Vec<SessionSummary>> {
        if !self.directory.exists() {
            return Ok(Vec::new());
        }

        let canonical_filter = cwd.and_then(|path| path.canonicalize().ok());
        let mut summaries = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(session) = serde_json::from_slice::<Session>(&bytes) else {
                continue;
            };
            if let Some(filter) = &canonical_filter
                && session.cwd.canonicalize().ok().as_ref() != Some(filter)
            {
                continue;
            }
            let preview = first_user_content(&session.messages)
                .map(compact_preview)
                .unwrap_or_default();
            // title/summary 为空的旧会话在这里惰性推导，无需迁移写盘。
            let title = if session.title.trim().is_empty() {
                first_user_content(&session.messages)
                    .map(derive_title)
                    .unwrap_or_default()
            } else {
                session.title.clone()
            };
            let summary = if session.summary.trim().is_empty() {
                derive_summary(&session.messages)
            } else {
                session.summary.clone()
            };
            summaries.push(SessionSummary {
                id: session.id,
                provider_id: session.provider_id,
                model: session.model,
                cwd: session.cwd,
                updated_at: session.updated_at,
                preview,
                title,
                title_manually_set: session.title_manually_set,
                pinned: session.pinned,
                summary,
            });
        }
        summaries.sort_by_key(|summary| Reverse(summary.updated_at));
        Ok(summaries)
    }

    fn path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

fn compact_preview(value: &str) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    single_line.chars().take(72).collect()
}

/// 首条真实用户消息的原文（跳过 internal 消息，如自动注入的指令）。
fn first_user_content(messages: &[ChatMessage]) -> Option<&str> {
    messages
        .iter()
        .find(|message| message.role == crate::Role::User && !message.internal)
        .map(|message| message.content.as_str())
}

/// 标题：首条用户消息压缩为一行，截断到 42 字符（与前端 deriveTitle 一致）。
fn derive_title(value: &str) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = single_line.trim();
    let mut chars = trimmed.chars();
    let head: String = chars.by_ref().take(42).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 多轮采样摘要：遍历 (user, assistant) 轮次，每轮取 user 前 40 字符 +
/// assistant 首尾各 48 字符，以 "；" 连接轮次。
/// 总长超 `SUMMARY_MAX_CHARS` 时保留首末轮完整、中间轮退化为只留 user 问题，
/// 保证多轮对话里中间轮次的核心主题（问题/结论）也能被检索命中。
fn derive_summary(messages: &[ChatMessage]) -> String {
    const ROUND_USER_CHARS: usize = 40;
    const ROUND_ASSISTANT_CHARS: usize = 48;

    struct Round {
        user: String,
        assistant: String,
    }

    let mut rounds: Vec<Round> = Vec::new();
    let mut pending_user: Option<String> = None;
    for message in messages {
        if message.compaction_summary {
            continue;
        }
        match message.role {
            crate::Role::User if !message.internal => {
                // 连续多条 user（旧会话/连续提问）：前一条作为“无回复”轮次保留，
                // 避免被覆盖丢失。
                if let Some(prev) = pending_user.take() {
                    rounds.push(Round {
                        user: prev,
                        assistant: String::new(),
                    });
                }
                pending_user = Some(
                    compact_preview(&message.content)
                        .chars()
                        .take(ROUND_USER_CHARS)
                        .collect(),
                );
            }
            crate::Role::Assistant if !message.content.trim().is_empty() => {
                if let Some(user) = pending_user.take() {
                    rounds.push(Round {
                        user,
                        assistant: summarize_assistant(&message.content, ROUND_ASSISTANT_CHARS),
                    });
                }
            }
            _ => {}
        }
    }
    if let Some(user) = pending_user {
        rounds.push(Round {
            user,
            assistant: String::new(),
        });
    }
    rounds.retain(|round| !round.user.is_empty() || !round.assistant.is_empty());
    if rounds.is_empty() {
        return String::new();
    }

    let rendered: Vec<String> = rounds
        .iter()
        .map(|round| {
            if round.assistant.is_empty() {
                round.user.clone()
            } else {
                format!("{} → {}", round.user, round.assistant)
            }
        })
        .collect();
    let full = rendered.join("；");
    if full.chars().count() <= SUMMARY_MAX_CHARS || rounds.len() <= 2 {
        return full.chars().take(SUMMARY_MAX_CHARS).collect();
    }
    // 超限：首末轮完整；中间轮只保留 user 问题（每轮最多 24 字符），
    // 按预算从最早的中间轮开始装，**末轮永远完整**——保证最新意图的
    // 尾部关键词不被硬截断（与「尾部关键词不漏检」的目标一致）。
    let first = rendered.first().cloned().unwrap_or_default();
    let last = rendered.last().cloned().unwrap_or_default();
    let mut budget = SUMMARY_MAX_CHARS.saturating_sub(first.chars().count() + last.chars().count());
    let mut middle: Vec<String> = Vec::new();
    for round in &rounds[1..rounds.len() - 1] {
        let user: String = round.user.chars().take(24).collect();
        let user_len = user.chars().count();
        if !user.is_empty() && budget >= user_len {
            budget -= user_len;
            middle.push(user);
        }
    }
    [first, middle.join("；"), last].join("；")
}

/// 摘要总长上限（字符）。
const SUMMARY_MAX_CHARS: usize = 800;

/// 助手回复摘要：首尾双侧采样（各 `head_tail` 字符）。
/// 只取开头会把长回复中后段的关键信息（技术词、结论）漏掉，
/// 导致检索命中不了——检索的价值正在于这些词。
fn summarize_assistant(content: &str, head_tail: usize) -> String {
    let single_line = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = single_line.trim();
    let len = s.chars().count();
    if len <= head_tail * 2 {
        return s.chars().take(head_tail * 2).collect();
    }
    let head: String = s.chars().take(head_tail).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(head_tail)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_lists_and_loads_sessions() {
        let home = tempfile::tempdir().expect("temporary home");
        let store = SessionStore::new(home.path());
        let mut session = Session::new("provider", "model", home.path().to_path_buf());
        session
            .messages
            .push(ChatMessage::user("inspect this project"));
        session
            .messages
            .push(ChatMessage::assistant("the build is green", Vec::new()));
        store.save(&session).expect("save session");

        let listed = store.list(Some(home.path())).expect("list sessions");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].preview, "inspect this project");
        assert_eq!(listed[0].title, "inspect this project");
        assert_eq!(
            listed[0].summary,
            "inspect this project → the build is green"
        );
        assert_eq!(store.load(session.id).expect("load session").model, "model");
        assert!(store.delete(session.id).expect("delete session"));
        assert!(!store.delete(session.id).expect("delete missing session"));
    }

    #[test]
    fn metadata_survives_restart_and_stale_checkpoint_save() {
        let home = tempfile::tempdir().expect("temporary home");
        let store = SessionStore::new(home.path());
        let mut stale = Session::new("provider", "model", home.path().to_path_buf());
        stale.messages.push(ChatMessage::user("original title"));
        store.save(&stale).expect("save initial session");

        store
            .update_metadata(stale.id, Some("用户标题"), Some(true))
            .expect("persist metadata");
        stale
            .messages
            .push(ChatMessage::assistant("done", Vec::new()));
        store.save(&stale).expect("save stale checkpoint");

        let loaded = SessionStore::new(home.path())
            .load(stale.id)
            .expect("load after restart");
        assert_eq!(loaded.title, "用户标题");
        assert!(loaded.title_manually_set);
        assert!(loaded.pinned);
        assert_eq!(loaded.messages.len(), 2);
        let listed = store.list(None).expect("list sessions");
        assert!(listed[0].pinned);
        assert!(listed[0].title_manually_set);
    }

    #[test]
    fn list_lazily_derives_title_and_summary_for_old_sessions() {
        let home = tempfile::tempdir().expect("temporary home");
        let store = SessionStore::new(home.path());
        // 模拟旧版本会话：无 title/summary 字段（serde default 兼容）。
        let mut session = Session::new("provider", "model", home.path().to_path_buf());
        session.title.clear();
        session.summary.clear();
        let long_first = format!("first message {}", "x".repeat(200));
        session.messages.push(ChatMessage::user(long_first.clone()));
        session
            .messages
            .push(ChatMessage::user("second user message, not the title"));
        session
            .messages
            .push(ChatMessage::assistant("finished the migration", Vec::new()));
        store.save(&session).expect("save session");

        // 反序列化旧 JSON（无 title/summary 键）仍然成功。
        let path = store.path(session.id);
        let mut value = serde_json::to_value(&session).expect("serialize session");
        value
            .as_object_mut()
            .expect("session object")
            .remove("title");
        value
            .as_object_mut()
            .expect("session object")
            .remove("summary");
        value
            .as_object_mut()
            .expect("session object")
            .remove("title_manually_set");
        value
            .as_object_mut()
            .expect("session object")
            .remove("pinned");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("pretty json"),
        )
        .expect("write old-style json");
        let loaded = store.load(session.id).expect("load old-style session");
        assert!(loaded.title.is_empty());
        assert!(loaded.summary.is_empty());
        assert!(!loaded.title_manually_set);
        assert!(!loaded.pinned);

        let listed = store.list(Some(home.path())).expect("list sessions");
        assert_eq!(listed.len(), 1);
        // 标题取首条 user 消息并截断到 42 字符 + 省略号。
        assert_eq!(listed[0].title.chars().count(), 43);
        assert!(listed[0].title.starts_with("first message"));
        assert!(listed[0].title.ends_with('…'));
        // 多轮采样：首轮 user、末尾 assistant 回复、以及中间轮 user 问题都应进摘要。
        assert!(listed[0].summary.contains("first message"));
        assert!(listed[0].summary.contains("finished the migration"));
        assert!(listed[0].summary.contains("second user message"));
    }

    #[test]
    fn derive_title_compresses_and_truncates() {
        assert_eq!(derive_title("  hello\n  world  "), "hello world");
        let long = "a".repeat(100);
        let title = derive_title(&long);
        assert_eq!(title.chars().count(), 43);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn derive_summary_links_every_user_assistant_round() {
        let messages = vec![
            ChatMessage::user("fix the parser"),
            ChatMessage::assistant("on it", Vec::new()),
            ChatMessage::user("also update tests"),
            ChatMessage::assistant("tests updated", Vec::new()),
        ];
        let summary = derive_summary(&messages);
        assert_eq!(
            summary,
            "fix the parser → on it；also update tests → tests updated"
        );
        // 无 assistant 时退化为首条 user。
        let only_user = vec![ChatMessage::user("just a note")];
        assert_eq!(derive_summary(&only_user), "just a note");
    }

    #[test]
    fn derive_summary_covers_middle_round_keywords() {
        // 3 轮对话：中间轮次的 user 问题必须进摘要，否则检索命中不了。
        let messages = vec![
            ChatMessage::user("Redis 缓存穿透是什么"),
            ChatMessage::assistant("穿透：查不存在的 key，方案是布隆过滤器", Vec::new()),
            ChatMessage::user("那缓存雪崩呢"),
            ChatMessage::assistant("雪崩：一批 key 同时过期，方案是 TTL 随机化", Vec::new()),
            ChatMessage::user("击穿和穿透怎么区分"),
            ChatMessage::assistant("击穿：热 key 失效瞬间并发打爆，方案是互斥锁", Vec::new()),
        ];
        let summary = derive_summary(&messages);
        assert!(
            summary.contains("缓存雪崩"),
            "中间轮 user 问题应进摘要，实际: {summary:?}"
        );
        assert!(
            summary.contains("击穿"),
            "末轮 user 问题应进摘要，实际: {summary:?}"
        );
        assert!(
            summary.contains("布隆过滤器"),
            "首轮 assistant 结论应进摘要"
        );
    }

    #[test]
    fn derive_summary_compresses_many_rounds_to_budget() {
        // 很多轮 + 长回复：总长不能爆掉 SUMMARY_MAX_CHARS，且首末轮保留。
        let mut messages = Vec::new();
        for i in 0..12 {
            messages.push(ChatMessage::user(format!("第 {i} 轮的问题是什么")));
            messages.push(ChatMessage::assistant(
                &format!("这是第 {i} 轮的回复，内容比较长，包含一些技术细节和结论。"),
                Vec::new(),
            ));
        }
        let summary = derive_summary(&messages);
        assert!(
            summary.chars().count() <= SUMMARY_MAX_CHARS,
            "摘要超长: {} 字符",
            summary.chars().count()
        );
        assert!(summary.contains("第 0 轮"), "首轮应保留");
        assert!(summary.contains("第 11 轮"), "末轮应保留");
        // 中间轮退化为 user 问题仍可检索。
        assert!(summary.contains("第 5 轮"), "中间轮 user 问题应保留");
    }

    #[test]
    fn derive_summary_keeps_last_round_tail_under_budget_pressure() {
        // 20 轮 + 长回复：压缩预算吃紧时**末轮尾部关键词必须保留**（防硬截断漏检）。
        const KEYWORD: &str = "末轮保留关键";
        let mut messages = Vec::new();
        for i in 0..20 {
            messages.push(ChatMessage::user(format!("第 {i} 轮的问题")));
            let reply = format!(
                "第 {i} 轮回复：{}……最后结论关键字是{KEYWORD}",
                "细节内容".repeat(30),
            );
            messages.push(ChatMessage::assistant(&reply, Vec::new()));
        }
        let summary = derive_summary(&messages);
        assert!(
            summary.chars().count() <= SUMMARY_MAX_CHARS,
            "摘要超长: {} 字符",
            summary.chars().count()
        );
        assert!(
            summary.ends_with(&format!("{KEYWORD}")),
            "末轮尾部关键词应保留（防硬截断），实际结尾: {:?}",
            summary.chars().rev().take(12).collect::<String>()
        );
    }

    #[test]
    fn derive_summary_captures_tail_keywords_of_long_reply() {
        // 长回复：关键信息（多进程）只出现在尾部，只取开头会漏掉。
        let mut long = String::from(
            "GIL 全称 Global Interpreter Lock，它让 CPython 同一时刻只有一个线程执行字节码。",
        );
        long.push_str(&"a".repeat(2000));
        long.push_str("需要的话我可以帮你写一个对比 GIL 影响、或用多进程/NumPy 加速的具体示例。");
        let messages = vec![
            ChatMessage::user("Python 的 GIL 是什么？影响什么场景"),
            ChatMessage::assistant(&long, Vec::new()),
        ];
        let summary = derive_summary(&messages);
        assert!(
            summary.contains("多进程"),
            "摘要应包含尾部关键词，实际: {summary:?}"
        );
        // 短回复双侧不重复、不截断过多。
        let short = vec![
            ChatMessage::user("hi"),
            ChatMessage::assistant("hello world", Vec::new()),
        ];
        assert_eq!(derive_summary(&short), "hi → hello world");
    }

    #[test]
    fn messages_get_unique_and_stable_ids() {
        let mut session = Session::new("provider", "model", PathBuf::from("/tmp"));
        session.messages.push(ChatMessage::user("first"));
        session
            .messages
            .push(ChatMessage::assistant("reply", Vec::new()));
        let id0 = session.messages[0].id.clone();
        let id1 = session.messages[1].id.clone();
        assert!(!id0.is_empty());
        assert!(!id1.is_empty());
        assert_ne!(id0, id1, "message ids should be unique");
    }

    #[test]
    fn edit_message_changes_content_only() {
        let mut session = Session::new("provider", "model", PathBuf::from("/tmp"));
        session.messages.push(ChatMessage::user("old question"));
        let id = session.messages[0].id.clone();
        session.edit_message(&id, "new question").expect("edit");
        assert_eq!(session.messages[0].content, "new question");
        assert_eq!(session.messages[0].role, crate::Role::User);
        assert!(session.edit_message("no-such-id", "x").is_err());
    }

    #[test]
    fn delete_assistant_removes_following_tool_results() {
        let mut session = Session::new("provider", "model", PathBuf::from("/tmp"));
        let user = ChatMessage::user("do something");
        let user_id = user.id.clone();
        session.messages.push(user);
        let assistant = ChatMessage::assistant("done", Vec::new());
        let assistant_id = assistant.id.clone();
        session.messages.push(assistant);
        session.messages.push(ChatMessage::tool("call-1", "result"));
        session
            .messages
            .push(ChatMessage::tool("call-2", "result2"));
        assert_eq!(session.messages.len(), 4);
        let removed = session.delete_message(&assistant_id).expect("delete");
        assert_eq!(
            removed, 3,
            "delete assistant should remove following tool results"
        );
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].id, user_id);
    }

    #[test]
    fn truncate_from_drops_message_and_the_rest() {
        let mut session = Session::new("provider", "model", PathBuf::from("/tmp"));
        let q1 = ChatMessage::user("q1");
        let a1 = ChatMessage::assistant("a1", Vec::new());
        let q2 = ChatMessage::user("q2");
        let a2 = ChatMessage::assistant("a2", Vec::new());
        let q2_id = q2.id.clone();
        session.messages.push(q1);
        session.messages.push(a1);
        session.messages.push(q2);
        session.messages.push(a2);
        let removed = session.truncate_from(&q2_id).expect("truncate");
        assert_eq!(removed, 2);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].content, "q1");
        assert_eq!(session.messages[1].content, "a1");
    }
}
