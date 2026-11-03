use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

const STALE_AFTER_DAYS: i64 = 7;
const MAX_PROMPT_CHARS: usize = 32_000;
const CORE_MEMORY_LIMIT: usize = 10;
const NEW_MEMORY_PROTECTION_DAYS: i64 = 14;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    #[default]
    User,
    Feedback,
    Project,
    Reference,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Local,
    Project,
    Global,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Memory {
    pub name: String,
    pub description: String,
    #[serde(rename = "type", default)]
    pub memory_type: MemoryType,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    #[serde(default)]
    pub hit_count: u64,
    #[serde(default)]
    pub last_triggered: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub content: String,
    #[serde(skip)]
    pub stale: bool,
    #[serde(skip)]
    pub scope: Option<MemoryScope>,
    #[serde(skip)]
    pub lifecycle: MemoryLifecycle,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLifecycle {
    #[default]
    Candidate,
    Stable,
    Core,
}

#[derive(Clone)]
pub struct MemoryManager {
    local_dir: PathBuf,
    project_dir: PathBuf,
    global_dir: PathBuf,
}

impl MemoryManager {
    pub fn new(home: &Path, project_path: &Path) -> Self {
        let project_key = format!(
            "{:x}",
            md5::compute(project_path.to_string_lossy().as_bytes())
        );
        Self {
            local_dir: project_path.join(".coomi").join("memory"),
            project_dir: home
                .join("projects")
                .join(&project_key[..12.min(project_key.len())])
                .join("memory"),
            global_dir: home.join("memory"),
        }
    }

    pub fn list(&self) -> Vec<Memory> {
        let mut seen = BTreeSet::new();
        let mut memories = Vec::new();
        for (scope, directory) in self.directories() {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            let mut paths = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension().and_then(|value| value.to_str()) == Some("md")
                        && path.file_name().and_then(|value| value.to_str()) != Some("MEMORY.md")
                })
                .collect::<Vec<_>>();
            paths.sort();
            for path in paths {
                let Ok(mut memory) = read_memory(&path) else {
                    continue;
                };
                if !seen.insert(memory.name.clone()) {
                    continue;
                }
                memory.scope = Some(scope);
                memory.stale = matches!(
                    memory.memory_type,
                    MemoryType::Project | MemoryType::Reference
                ) && Utc::now().signed_duration_since(memory.updated)
                    > Duration::days(STALE_AFTER_DAYS);
                memories.push(memory);
            }
        }
        assign_lifecycle(&mut memories);
        memories.sort_by(|left, right| {
            lifecycle_rank(right.lifecycle)
                .cmp(&lifecycle_rank(left.lifecycle))
                .then_with(|| right.hit_count.cmp(&left.hit_count))
                .then_with(|| left.created.cmp(&right.created))
        });
        memories
    }

    pub fn get(&self, name: &str) -> Option<Memory> {
        self.list().into_iter().find(|memory| memory.name == name)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Memory> {
        let terms = query
            .split(|character: char| {
                !character.is_alphanumeric() && character != '_' && character != '-'
            })
            .filter(|term| term.chars().count() >= 2)
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let mut scored = self
            .list()
            .into_iter()
            .filter_map(|memory| {
                let name = memory.name.to_lowercase();
                let description = memory.description.to_lowercase();
                let content = memory.content.to_lowercase();
                let score = terms.iter().fold(0usize, |score, term| {
                    score
                        + usize::from(name.contains(term)) * 5
                        + usize::from(description.contains(term)) * 3
                        + usize::from(content.contains(term))
                });
                (score > 0).then_some((score, memory))
            })
            .collect::<Vec<_>>();
        scored.sort_by_key(|item| std::cmp::Reverse(item.0));
        scored
            .into_iter()
            .take(limit.max(1))
            .map(|(_, memory)| memory)
            .collect()
    }

    pub fn save(
        &self,
        scope: MemoryScope,
        name: &str,
        description: &str,
        memory_type: MemoryType,
        content: &str,
    ) -> Result<PathBuf> {
        validate_name(name)?;
        let directory = self.directory(scope);
        fs::create_dir_all(directory)?;
        let path = directory.join(format!("{name}.md"));
        let existing = read_memory(&path).ok();
        let now = Utc::now();
        let memory = Memory {
            name: name.to_owned(),
            description: description.to_owned(),
            memory_type,
            created: existing.as_ref().map_or(now, |memory| memory.created),
            updated: now,
            hit_count: existing.as_ref().map_or(0, |memory| memory.hit_count),
            last_triggered: existing.as_ref().and_then(|memory| memory.last_triggered),
            content: content.to_owned(),
            stale: false,
            scope: Some(scope),
            lifecycle: MemoryLifecycle::Candidate,
        };
        fs::write(&path, render_memory(&memory))
            .with_context(|| format!("failed to save memory {}", path.display()))?;
        self.refresh_index()?;
        Ok(path)
    }

    pub fn delete(&self, name: &str) -> Result<bool> {
        validate_name(name)?;
        for (_, directory) in self.directories() {
            let path = directory.join(format!("{name}.md"));
            if path.is_file() {
                fs::remove_file(&path)?;
                self.refresh_index()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn prompt_context(&self) -> String {
        let mut output = String::new();
        for memory in self.list().into_iter().filter(|memory| {
            !memory.stale
                && !(memory.name.starts_with("session-")
                    && memory.description == "会话压缩前的完整工作进度，用于压缩后恢复上下文方向")
        }) {
            let entry = format!(
                "### {} [{:?}, {} hits]\n_{}_\n\n{}\n\n",
                memory.name, memory.lifecycle, memory.hit_count, memory.description, memory.content
            );
            if output.len().saturating_add(entry.len()) > MAX_PROMPT_CHARS {
                break;
            }
            output.push_str(&entry);
        }
        output
    }

    /// Observe one direct user message. Matching and counters are deterministic;
    /// the model is never responsible for remembering to update statistics.
    pub fn observe_user_message(&self, message: &str) -> Result<Vec<String>> {
        let sanitized = sanitize_memory_text(message);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }
        let now = Utc::now();
        let input_grams = text_bigrams(&sanitized);
        let mut hits = Vec::new();
        for (_, directory) in self.directories() {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for path in entries.flatten().map(|entry| entry.path()).filter(|path| {
                path.extension().and_then(|value| value.to_str()) == Some("md")
                    && path.file_name().and_then(|value| value.to_str()) != Some("MEMORY.md")
            }) {
                let Ok(mut memory) = read_memory(&path) else {
                    continue;
                };
                if memory_matches(&input_grams, &memory) {
                    memory.hit_count = memory.hit_count.saturating_add(1);
                    memory.last_triggered = Some(now);
                    memory.updated = now;
                    fs::write(&path, render_memory(&memory))?;
                    hits.push(memory.name);
                }
            }
        }

        if is_memory_signal(message) {
            let digest = format!("{:x}", md5::compute(sanitized.as_bytes()));
            let name = format!("reminder-{}", &digest[..12]);
            if self.get(&name).is_none() {
                self.save(
                    MemoryScope::Global,
                    &name,
                    &sanitized.chars().take(80).collect::<String>(),
                    if contains_correction_signal(message) {
                        MemoryType::Feedback
                    } else {
                        MemoryType::User
                    },
                    &sanitized,
                )?;
                let path = self.global_dir.join(format!("{name}.md"));
                if let Ok(mut memory) = read_memory(&path) {
                    memory.hit_count = 1;
                    memory.last_triggered = Some(now);
                    fs::write(path, render_memory(&memory))?;
                }
                hits.push(name);
            }
        }
        self.refresh_index()?;
        Ok(hits)
    }

    pub fn report(&self) -> String {
        let memories = self.list();
        if memories.is_empty() {
            return "当前没有 Coomi 内建持久记忆。此指令不会读取任何 MCP、Skill 或第三方记忆扩展。"
                .into();
        }
        let mut output = format!(
            "当前共有 {} 条 Coomi 内建持久记忆（不含任何 MCP、Skill 或第三方记忆扩展）。核心层最多保留 {} 条，排序由生命周期、命中次数和创建时间共同决定。\n\n",
            memories.len(),
            CORE_MEMORY_LIMIT
        );
        for memory in memories {
            output.push_str(&format!(
                "- {} [{:?}/{:?}]：命中 {} 次，最近命中 {}。{}\n  {}\n",
                memory.name,
                memory.lifecycle,
                memory.scope.unwrap_or(MemoryScope::Project),
                memory.hit_count,
                memory.last_triggered.map_or_else(
                    || "从未".into(),
                    |time| time.format("%Y-%m-%d %H:%M").to_string()
                ),
                memory.description,
                memory.content,
            ));
        }
        output
    }

    pub fn refresh_index(&self) -> Result<()> {
        let directory = if self.local_dir.is_dir() {
            &self.local_dir
        } else {
            &self.project_dir
        };
        fs::create_dir_all(directory)?;
        let mut lines = vec![
            "# Memory Index".to_owned(),
            "> Auto-generated. Local entries override project and global entries.".to_owned(),
            String::new(),
        ];
        for memory in self.list() {
            lines.push(format!(
                "- [{}](./{}.md) - {}{}",
                memory.name,
                memory.name,
                memory.description,
                if memory.stale { " [stale]" } else { "" }
            ));
        }
        fs::write(directory.join("MEMORY.md"), lines.join("\n"))?;
        Ok(())
    }

    fn directories(&self) -> [(MemoryScope, &Path); 3] {
        [
            (MemoryScope::Local, &self.local_dir),
            (MemoryScope::Project, &self.project_dir),
            (MemoryScope::Global, &self.global_dir),
        ]
    }

    fn directory(&self, scope: MemoryScope) -> &Path {
        match scope {
            MemoryScope::Local => &self.local_dir,
            MemoryScope::Project => &self.project_dir,
            MemoryScope::Global => &self.global_dir,
        }
    }
}

fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name.len() <= 80
            && name.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            ),
        "memory name must use 1-80 ASCII letters, numbers, hyphens, or underscores"
    );
    Ok(())
}

fn read_memory(path: &Path) -> Result<Memory> {
    let text = fs::read_to_string(path)?;
    let rest = text
        .strip_prefix("---\n")
        .context("memory has no frontmatter")?;
    let (frontmatter, content) = rest
        .split_once("\n---\n")
        .context("memory frontmatter is not closed")?;
    let mut memory: Memory = serde_yaml::from_str(frontmatter)?;
    memory.content = content.trim().to_owned();
    memory.lifecycle = MemoryLifecycle::Candidate;
    Ok(memory)
}

fn lifecycle_rank(lifecycle: MemoryLifecycle) -> u8 {
    match lifecycle {
        MemoryLifecycle::Candidate => 0,
        MemoryLifecycle::Stable => 1,
        MemoryLifecycle::Core => 2,
    }
}

fn assign_lifecycle(memories: &mut [Memory]) {
    let now = Utc::now();
    let mut eligible = memories
        .iter()
        .enumerate()
        .filter(|(_, memory)| memory.hit_count >= 3)
        .map(|(index, memory)| {
            let age = memory.last_triggered.map_or(365.0, |time| {
                now.signed_duration_since(time).num_hours().max(0) as f64 / 24.0
            });
            let score = ((memory.hit_count + 1) as f64).log2() * (-age / 30.0).exp();
            (index, score)
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| right.1.total_cmp(&left.1));
    let core = eligible
        .into_iter()
        .take(CORE_MEMORY_LIMIT)
        .map(|(index, _)| index)
        .collect::<HashSet<_>>();
    for (index, memory) in memories.iter_mut().enumerate() {
        memory.lifecycle = if core.contains(&index) {
            MemoryLifecycle::Core
        } else if memory.hit_count >= 2
            || now.signed_duration_since(memory.created)
                <= Duration::days(NEW_MEMORY_PROTECTION_DAYS)
        {
            MemoryLifecycle::Stable
        } else {
            MemoryLifecycle::Candidate
        };
    }
}

fn is_memory_signal(message: &str) -> bool {
    let explicit_signal = [
        "请记住",
        "记住",
        "以后请",
        "永远不要",
        "必须",
        "你可以",
        "你有",
        "你做错了",
        "下次应该",
        "我希望你",
        "默认",
        "优先",
    ]
    .iter()
    .any(|signal| message.contains(signal));
    explicit_signal || (message.contains("不是") && message.contains("而是"))
}

fn contains_correction_signal(message: &str) -> bool {
    ["你做错了", "下次应该", "不要说你不能"]
        .iter()
        .any(|signal| message.contains(signal))
        || (message.contains("不是") && message.contains("而是"))
}

fn sanitize_memory_text(message: &str) -> String {
    message
        .split_whitespace()
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            if part.contains('@')
                || lower.contains("token=")
                || lower.contains("api_key")
                || lower.contains("apikey")
                || lower.starts_with("sk-")
            {
                "[已脱敏]".to_owned()
            } else if part.starts_with('/') || part.contains(":\\") {
                "[路径]".to_owned()
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1_000)
        .collect()
}

fn text_bigrams(text: &str) -> HashSet<String> {
    let normalized = text
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect::<Vec<_>>();
    normalized
        .windows(2)
        .map(|pair| pair.iter().collect())
        .collect()
}

fn memory_matches(input: &HashSet<String>, memory: &Memory) -> bool {
    let target = text_bigrams(&format!(
        "{} {} {}",
        memory.name, memory.description, memory.content
    ));
    if target.is_empty() {
        return false;
    }
    let overlap = input.intersection(&target).count();
    overlap >= 3 && overlap * 3 >= target.len().min(30)
}

fn render_memory(memory: &Memory) -> String {
    let frontmatter = serde_yaml::to_string(memory).unwrap_or_default();
    format!("---\n{}---\n\n{}\n", frontmatter, memory.content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_for_test(name: &str, created: DateTime<Utc>, hit_count: u64) -> Memory {
        Memory {
            name: name.to_owned(),
            description: name.to_owned(),
            memory_type: MemoryType::User,
            created,
            updated: created,
            hit_count,
            last_triggered: Some(created),
            content: name.to_owned(),
            stale: false,
            scope: Some(MemoryScope::Global),
            lifecycle: MemoryLifecycle::Candidate,
        }
    }

    #[test]
    fn local_memory_overrides_project_and_global() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let manager = MemoryManager::new(home.path(), project.path());
        manager
            .save(
                MemoryScope::Global,
                "preference",
                "global",
                MemoryType::User,
                "dark",
            )
            .expect("global memory");
        manager
            .save(
                MemoryScope::Local,
                "preference",
                "local",
                MemoryType::User,
                "light",
            )
            .expect("local memory");
        let memories = manager.list();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "light");
        assert_eq!(memories[0].scope, Some(MemoryScope::Local));
    }

    #[test]
    fn legacy_frontmatter_loads_with_zero_hits() {
        let directory = tempfile::tempdir().expect("memory directory");
        let path = directory.path().join("legacy.md");
        fs::write(
            &path,
            "---\nname: legacy\ndescription: old format\ntype: user\ncreated: 2026-01-01T00:00:00Z\nupdated: 2026-01-02T00:00:00Z\n---\n\nlegacy content\n",
        )
        .expect("legacy memory");
        let memory = read_memory(&path).expect("load legacy memory");
        assert_eq!(memory.hit_count, 0);
        assert_eq!(memory.last_triggered, None);
        assert_eq!(memory.content, "legacy content");
    }

    #[test]
    fn observation_updates_hits_without_model_assistance() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let manager = MemoryManager::new(home.path(), project.path());
        manager
            .save(
                MemoryScope::Global,
                "absolute-paths",
                "export files with absolute paths",
                MemoryType::User,
                "所有导出文件必须使用完整路径",
            )
            .expect("save memory");
        let hits = manager
            .observe_user_message("所有导出文件必须使用完整路径")
            .expect("observe message");
        assert!(hits.contains(&"absolute-paths".to_owned()));
        assert_eq!(manager.get("absolute-paths").expect("memory").hit_count, 1);
    }

    #[test]
    fn core_is_limited_and_new_low_hit_memory_is_protected() {
        let now = Utc::now();
        let mut memories = (0..12)
            .map(|index| memory_for_test(&format!("frequent-{index}"), now, 3 + index))
            .collect::<Vec<_>>();
        memories.push(memory_for_test("new-reminder", now, 0));
        assign_lifecycle(&mut memories);
        assert_eq!(
            memories
                .iter()
                .filter(|memory| memory.lifecycle == MemoryLifecycle::Core)
                .count(),
            CORE_MEMORY_LIMIT
        );
        assert_eq!(
            memories
                .iter()
                .find(|memory| memory.name == "new-reminder")
                .expect("new memory")
                .lifecycle,
            MemoryLifecycle::Stable
        );
    }

    #[test]
    fn plain_negation_does_not_create_a_memory_signal() {
        assert!(!is_memory_signal("今天不是晴天"));
        assert!(is_memory_signal("你不是不能导出，而是应该使用完整路径"));
    }
}
