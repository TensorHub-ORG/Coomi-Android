mod hooks;

use anyhow::Context;
use anyhow::Result;
use clap::ValueEnum;
use regex::Regex;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

pub use hooks::HookEvent;
pub use hooks::HookOutcome;
pub use hooks::HookRunner;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AccessMode {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

impl AccessMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::FullAccess => "full-access",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    Allow,
    Ask(String),
    Deny(String),
}

#[derive(Clone, Debug)]
pub struct SecurityPolicy {
    workspace: PathBuf,
    allowed_roots: Vec<PathBuf>,
    mode: AccessMode,
    /// 工作区内的私有屏蔽区（如 .coomi/sessions）：任何模式（含 FullAccess）都不可访问。
    blocked: Vec<PathBuf>,
    readable_files: Vec<PathBuf>,
    /// 屏蔽区的其它路径写法（词法规范化/原始形式），供 shell 命令文本匹配用
    /// （Android 上 /data/data 与 /data/user/0 互为符号链接，两种写法都要拦）。
    blocked_aliases: Vec<PathBuf>,
}

impl SecurityPolicy {
    pub fn new(workspace: impl AsRef<Path>, mode: AccessMode) -> Result<Self> {
        let workspace = workspace
            .as_ref()
            .canonicalize()
            .with_context(|| format!("invalid workspace {}", workspace.as_ref().display()))?;
        Ok(Self {
            workspace,
            allowed_roots: Vec::new(),
            mode,
            blocked: Vec::new(),
            readable_files: Vec::new(),
            blocked_aliases: Vec::new(),
        })
    }

    /// Allow a single engine-selected file for read tools only. Shell and writes
    /// retain the blocked-directory policy; siblings never inherit this grant.
    pub fn with_readable_file(mut self, path: PathBuf) -> Self {
        if path.is_file() {
            if let Ok(path) = path.canonicalize() {
                self.blocked.push(path.clone());
                self.blocked_aliases.push(path.clone());
                self.readable_files.push(path);
            }
        }
        self
    }

    pub fn with_blocked(mut self, blocked: impl IntoIterator<Item = PathBuf>) -> Self {
        // 与工具侧 resolve_path 的规范化保持一致：存在则 canonicalize（Android 上
        // /data/data 是 /data/user/0 的符号链接），不存在则做词法规范化；
        // 同时保留多种写法供 shell 命令文本匹配（~、$HOME、相对形式）。
        self.blocked = blocked
            .into_iter()
            .filter_map(|path| {
                let normalized = normalize_path(&path).ok()?;
                self.blocked_aliases.push(normalized.clone());
                if normalized.exists() {
                    let canonical = normalized.canonicalize().unwrap_or_else(|_| normalized);
                    self.blocked_aliases.push(canonical.clone());
                    Some(canonical)
                } else {
                    Some(normalized)
                }
            })
            .collect();
        // 追加常见别名写法：~/.coomi/*、$HOME/.coomi/*、${HOME}/.coomi/* 以及
        // 相对形式 .coomi/{name}（前提是路径含名为 .coomi 的祖先，即引擎私有目录）。
        let home_forms: Vec<PathBuf> = self
            .blocked_aliases
            .iter()
            .filter_map(|path| {
                let ancestors = path.ancestors().collect::<Vec<_>>();
                let coomi = ancestors.iter().position(|component| {
                    component.file_name().and_then(|n| n.to_str()) == Some(".coomi")
                })?;
                let relative = ancestors[..=coomi]
                    .iter()
                    .rev()
                    .map(|component| {
                        component
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect::<Vec<_>>()
                    .join("/");
                let tail = ancestors[..coomi]
                    .iter()
                    .rev()
                    .map(|component| {
                        component
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect::<Vec<_>>();
                let mut forms = vec![
                    format!("~/{relative}"),
                    format!("$HOME/{relative}"),
                    format!("${{HOME}}/{relative}"),
                ];
                // .coomi/sessions 等相对形式（去掉 home 前缀）
                forms.push(relative.clone());
                // home 也可能以 ~/ 开头：~/.coomi/...
                if let Some(last) = tail.last() {
                    forms.push(format!("~/{last}/{relative}"));
                    forms.push(format!("$HOME/{last}/{relative}"));
                }
                Some(forms)
            })
            .flatten()
            .map(PathBuf::from)
            .collect();
        self.blocked_aliases.extend(home_forms);
        self.blocked_aliases.sort();
        self.blocked_aliases.dedup();
        self
    }
    pub fn with_additional_blocked(self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut combined = self.blocked.clone();
        combined.extend(paths);
        self.with_blocked(combined)
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn mode(&self) -> AccessMode {
        self.mode
    }

    /// Add controlled runtime roots (for example Proot's persistent home) to
    /// the file-tool allowlist without weakening the active access mode.
    pub fn with_allowed_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.allowed_roots
            .extend(roots.into_iter().filter_map(|path| {
                normalize_path(&path).ok().and_then(|path| {
                    if path.exists() {
                        path.canonicalize().ok()
                    } else {
                        Some(path)
                    }
                })
            }));
        self.allowed_roots.sort();
        self.allowed_roots.dedup();
        self
    }

    pub fn resolve_path(&self, value: impl AsRef<Path>) -> Result<PathBuf> {
        let value = value.as_ref();
        let joined = if value.is_absolute() {
            value.to_path_buf()
        } else {
            self.workspace.join(value)
        };
        resolve_symlinks(&normalize_path(&joined)?)
    }

    pub fn assess_read(&self, path: &Path) -> Decision {
        self.assess_path(path, false)
    }

    pub fn assess_write(&self, path: &Path) -> Decision {
        self.assess_path(path, true)
    }

    pub fn assess_shell(&self, command: &str) -> Decision {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return Decision::Deny("empty shell command".into());
        }

        // 全局会话记忆关闭时：命令文本引用私有屏蔽区（~/.coomi 的会话/配置/记忆目录）
        // 一律拒绝——shell 是文件工具之外唯一能读到这些路径的通道。
        for alias in &self.blocked_aliases {
            if trimmed.contains(alias.to_string_lossy().as_ref()) {
                return Decision::Deny("命令引用了被「全局会话记忆」策略屏蔽的私有目录".into());
            }
        }
        // 兜底：屏蔽区生效时，含 ~ / $HOME / ${HOME} 的写法无法静态判定，
        // 转人工确认（防止 `cat ~/.coomi/sessions/...` 之类绕过字面匹配）。
        if !self.blocked_aliases.is_empty()
            && (trimmed.contains('~') || trimmed.contains("$HOME") || trimmed.contains("${HOME}"))
        {
            return Decision::Ask(
                "命令使用了 ~ / $HOME 引用，可能访问被「全局会话记忆」屏蔽的私有目录，请确认"
                    .into(),
            );
        }

        if destructive_command().is_match(trimmed) {
            return match self.mode {
                AccessMode::FullAccess => {
                    Decision::Ask("command may delete or overwrite data".into())
                }
                _ => Decision::Deny("destructive command is blocked by the active policy".into()),
            };
        }

        match self.mode {
            AccessMode::FullAccess => Decision::Allow,
            AccessMode::WorkspaceWrite => {
                Decision::Ask("shell commands can change files or start processes".into())
            }
            AccessMode::ReadOnly if read_only_command().is_match(trimmed) => Decision::Allow,
            AccessMode::ReadOnly => Decision::Deny("command is not recognized as read-only".into()),
        }
    }

    fn assess_path(&self, path: &Path, write: bool) -> Decision {
        let Ok(path) = normalize_path(path).and_then(|path| resolve_symlinks(&path)) else {
            return Decision::Deny("path could not be normalized".into());
        };
        // 私有屏蔽区优先于权限模式：会话/配置/记忆目录在全局会话记忆关闭时
        // 对工具完全不可见（FullAccess 也一样被拦）。
        if !write && self.readable_files.contains(&path) {
            return Decision::Allow;
        }
        for blocked in &self.blocked {
            if path.starts_with(blocked) {
                return Decision::Deny(
                    "该路径属于会话/配置私有区，已被「全局会话记忆」策略屏蔽".into(),
                );
            }
        }
        if self.mode == AccessMode::FullAccess {
            return Decision::Allow;
        }
        if !path.starts_with(&self.workspace)
            && !self.allowed_roots.iter().any(|root| path.starts_with(root))
        {
            return Decision::Deny(format!(
                "path is outside allowed workspace/runtime roots (workspace: {})",
                self.workspace.display()
            ));
        }
        if write && self.mode == AccessMode::ReadOnly {
            return Decision::Deny("write blocked by read-only policy".into());
        }
        Decision::Allow
    }
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    anyhow::bail!("path escapes its root")
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn resolve_symlinks(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve path {}", path.display()));
    }

    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .context("path has no existing ancestor")?
            .to_os_string();
        missing.push(name);
        anyhow::ensure!(existing.pop(), "path has no existing ancestor");
    }
    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("failed to resolve path {}", existing.display()))?;
    for name in missing.into_iter().rev() {
        resolved.push(name);
    }
    normalize_path(&resolved)
}

fn destructive_command() -> Regex {
    Regex::new(
        r"(?i)(^|[;&|]\s*)(rm\s+-[^\r\n]*r|remove-item\b[^\r\n]*-recurse|rmdir\s+/s|del\s+/[a-z]*[sq]|git\s+reset\s+--hard|git\s+clean\s+-[^\r\n]*f|format\s+[a-z]:)",
    )
    .expect("valid destructive command regex")
}

fn read_only_command() -> Regex {
    Regex::new(
        r"(?i)^(pwd|ls\b|dir\b|get-childitem\b|get-content\b|type\b|cat\b|rg\b|grep\b|findstr\b|git\s+(status|diff|log|show|branch\s+--show-current)\b)",
    )
    .expect("valid read-only command regex")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auxiliary_parent_exception_is_exact_and_read_only() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("sessions");
        std::fs::create_dir_all(&private).unwrap();
        let parent = private.join("parent.json");
        let sibling = private.join("other.json");
        std::fs::write(&parent, "parent transcript").unwrap();
        std::fs::write(&sibling, "unrelated transcript").unwrap();
        let policy = SecurityPolicy::new(root.path(), AccessMode::FullAccess)
            .unwrap()
            .with_blocked([private.clone()])
            .with_readable_file(parent.clone());
        assert!(matches!(policy.assess_read(&parent), Decision::Allow));
        assert!(matches!(policy.assess_write(&parent), Decision::Deny(_)));
        assert!(matches!(policy.assess_read(&sibling), Decision::Deny(_)));
        assert!(matches!(policy.assess_read(&private), Decision::Deny(_)));
        assert!(matches!(
            policy.assess_shell(&format!("cat {}", parent.display())),
            Decision::Deny(_)
        ));
        let unrelated = SecurityPolicy::new(root.path(), AccessMode::FullAccess)
            .unwrap()
            .with_blocked([private]);
        assert!(matches!(unrelated.assess_read(&parent), Decision::Deny(_)));
    }

    #[test]
    fn workspace_policy_blocks_escape_and_allows_local_write() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let policy = SecurityPolicy::new(workspace.path(), AccessMode::WorkspaceWrite)
            .expect("security policy");
        let local = policy.resolve_path("src/main.rs").expect("local path");
        assert_eq!(policy.assess_write(&local), Decision::Allow);

        let outside = workspace
            .path()
            .parent()
            .expect("parent")
            .join("outside.txt");
        assert!(matches!(policy.assess_write(&outside), Decision::Deny(_)));
    }

    #[test]
    fn read_only_shell_policy_fails_closed() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let policy =
            SecurityPolicy::new(workspace.path(), AccessMode::ReadOnly).expect("security policy");
        assert_eq!(policy.assess_shell("git status"), Decision::Allow);
        assert!(matches!(
            policy.assess_shell("cargo fmt"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            policy.assess_shell("Remove-Item -Recurse src"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn blocked_private_dirs_are_denied_even_in_full_access() {
        // 全局会话记忆关闭时：会话/配置私有目录在 FullAccess 下也不可访问。
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let private = workspace.path().join(".coomi").join("sessions");
        std::fs::create_dir_all(&private).expect("create private dir");
        let policy = SecurityPolicy::new(workspace.path(), AccessMode::FullAccess)
            .expect("security policy")
            .with_blocked([private.clone()]);
        let probe = private.join("session.json");
        std::fs::write(&probe, b"{}").expect("write probe");
        // 与生产链路一致：工具侧先 resolve_path（canonicalize）再 assess_read。
        let probe = policy.resolve_path(&probe).expect("resolve probe");
        assert!(matches!(policy.assess_read(&probe), Decision::Deny(_)));
        // 工作区内其它路径在 FullAccess 下仍可正常读写。
        let other = workspace.path().join("notes.txt");
        std::fs::write(&other, b"hi").expect("write other");
        assert_eq!(policy.assess_read(&other), Decision::Allow);
        assert_eq!(policy.assess_write(&other), Decision::Allow);
        // shell 通道同样被屏蔽：引用私有目录的命令一律拒绝。
        let shell = format!("cat {}", private.display());
        assert!(matches!(policy.assess_shell(&shell), Decision::Deny(_)));
        let ok_shell = "cat notes.txt".to_string();
        assert_eq!(policy.assess_shell(&ok_shell), Decision::Allow);
        // 常见别名写法（~/.coomi/sessions、$HOME 形式）也要拦截。
        let tilde = format!("cat ~/.coomi/sessions/session.json");
        assert!(matches!(policy.assess_shell(&tilde), Decision::Deny(_)));
        let home_var = "cat $HOME/.coomi/sessions/session.json".to_string();
        assert!(matches!(policy.assess_shell(&home_var), Decision::Deny(_)));
        // 无法静态判定的 ~ / $HOME 引用走人工确认（Ask）。
        let ask_shell = "ls ~/project".to_string();
        assert!(matches!(policy.assess_shell(&ask_shell), Decision::Ask(_)));
    }
}
