//! Durable, per-user-turn file history. This never resets Git or scans a workspace.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

static JOURNAL_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MAX_FILE: usize = 32 * 1024 * 1024;
const MAX_TURN: u64 = 128 * 1024 * 1024;

#[derive(Clone)]
pub struct FileJournal {
    directory: PathBuf,
    workspace: PathBuf,
    home: PathBuf,
    turn_id: String,
}

pub struct FileEdit {
    pub path: PathBuf,
    pub before: Option<Vec<u8>>,
    pub after: Option<Vec<u8>>,
    pub after_mode: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Version {
    hash: String,
    size: u64,
    mode: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct Change {
    before: Option<Version>,
    after: Option<Version>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Journal {
    workspace: PathBuf,
    turn_id: String,
    changes: BTreeMap<PathBuf, Change>,
    warnings: Vec<String>,
    pending: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileUndoEntry {
    pub path: String,
    pub action: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileUndoPreview {
    pub turn_id: String,
    pub workspace: String,
    pub available: bool,
    pub files: Vec<FileUndoEntry>,
    pub conflicts: Vec<String>,
    pub warnings: Vec<String>,
    pub revision: String,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl FileJournal {
    pub fn new(home: &Path, workspace: &Path, session: &str, turn: &str) -> Result<Self> {
        let session = Uuid::parse_str(session).context("invalid journal session")?;
        Ok(Self {
            directory: home
                .join("turn-files")
                .join(session.to_string())
                .join(digest(turn.as_bytes())),
            workspace: workspace.canonicalize().context("工作目录不可用")?,
            home: home.canonicalize()?,
            turn_id: turn.to_owned(),
        })
    }

    pub fn ensure(&self) -> Result<()> {
        let _guard = JOURNAL_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        self.ensure_inner()
    }

    fn ensure_inner(&self) -> Result<()> {
        if self.path().exists() {
            self.load()?;
            return Ok(());
        }
        self.save(&Journal {
            workspace: self.workspace.clone(),
            turn_id: self.turn_id.clone(),
            changes: BTreeMap::new(),
            warnings: Vec::new(),
            pending: false,
        })
    }

    fn path(&self) -> PathBuf {
        self.directory.join("journal.json")
    }
    fn load(&self) -> Result<Journal> {
        let record: Journal = serde_json::from_slice(&fs::read(self.path())?)?;
        ensure!(
            record.workspace == self.workspace && record.turn_id == self.turn_id,
            "恢复记录与当前工作目录不匹配"
        );
        Ok(record)
    }
    fn save(&self, record: &Journal) -> Result<()> {
        fs::create_dir_all(&self.directory)?;
        atomic_write(&self.path(), &serde_json::to_vec(record)?, None)
    }

    pub fn mark_untracked(&self, reason: &str) -> Result<()> {
        let _guard = JOURNAL_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        self.ensure_inner()?;
        let mut record = self.load()?;
        if !record.warnings.iter().any(|item| item == reason) {
            record.warnings.push(reason.to_owned());
            self.save(&record)?;
        }
        Ok(())
    }

    /// Only regular workspace files are journaled. A symlink is never followed
    /// during restoration, including when a parent was replaced after the turn.
    pub fn relative_path(&self, path: &Path) -> Result<PathBuf> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        };
        let relative = path
            .strip_prefix(&self.workspace)
            .context("文件位于本轮工作目录之外")?;
        ensure!(!relative.as_os_str().is_empty(), "不能恢复工作目录本身");
        ensure!(!path.starts_with(&self.home), "应用数据目录不参与文件回撤");
        let mut current = self.workspace.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                bail!("恢复路径包含无效部分");
            };
            ensure!(
                !name.to_string_lossy().eq_ignore_ascii_case(".git"),
                "Git 元数据不参与文件回撤"
            );
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(meta) => ensure!(
                    !meta.file_type().is_symlink(),
                    "恢复路径已变为符号链接：{}",
                    relative.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(relative.to_path_buf())
    }

    fn capture(&self, relative: &Path, persist: bool) -> Result<Option<Version>> {
        let path = self.workspace.join(relative);
        self.relative_path(&path)?;
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(meta.is_file(), "目标已不是普通文件：{}", relative.display());
        ensure!(
            meta.len() <= MAX_FILE as u64,
            "文件超过回撤备份上限（32 MiB）：{}",
            relative.display()
        );
        let bytes = fs::read(&path)?;
        ensure!(bytes.len() <= MAX_FILE, "文件超过回撤备份上限");
        let version = Version {
            hash: digest(&bytes),
            size: bytes.len() as u64,
            mode: file_mode(&meta),
        };
        if persist {
            self.blob_save(&version, &bytes)?;
        }
        Ok(Some(version))
    }

    fn blob_save(&self, version: &Version, bytes: &[u8]) -> Result<()> {
        let path = self.directory.join(&version.hash);
        if !path.exists() {
            atomic_write(&path, bytes, None)?;
        }
        Ok(())
    }
    fn blob(&self, version: &Version) -> Result<Vec<u8>> {
        ensure!(
            version.hash.len() == 64 && version.hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "无效的文件备份标识"
        );
        let bytes = fs::read(self.directory.join(&version.hash)).context("文件备份缺失")?;
        ensure!(
            digest(&bytes) == version.hash && bytes.len() as u64 == version.size,
            "文件备份校验失败"
        );
        Ok(bytes)
    }

    /// Record the exact intended contents before writing. A crash leaves a
    /// pending record, so a later undo cannot silently assume execution finished.
    pub fn apply(&self, edits: Vec<FileEdit>) -> Result<()> {
        let _guard = JOURNAL_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        self.ensure_inner()?;
        let previous = self.load()?;
        ensure!(
            !previous.pending,
            "上次文件操作未完整结束，请先处理回撤记录"
        );
        ensure!(
            !self.directory.join("restore-pending.json").exists(),
            "上次回撤异常中断，请先核对文件与保留的备份"
        );
        let mut record = previous.clone();
        let mut updates = BTreeMap::new();
        for edit in edits {
            let relative = self.relative_path(&edit.path)?;
            ensure!(
                !updates.contains_key(&relative),
                "补丁重复修改同一路径，请拆分操作"
            );
            let current = self.capture(&relative, true)?;
            ensure!(
                current.as_ref().map(|v| &v.hash)
                    == edit.before.as_ref().map(|b| digest(b)).as_ref(),
                "文件在编辑前已变化：{}",
                relative.display()
            );
            let after = match edit.after {
                Some(bytes) => {
                    ensure!(bytes.len() <= MAX_FILE, "文件超过回撤备份上限（32 MiB）");
                    let version = Version {
                        hash: digest(&bytes),
                        size: bytes.len() as u64,
                        mode: edit
                            .after_mode
                            .unwrap_or_else(|| current.as_ref().map_or(default_mode(), |v| v.mode)),
                    };
                    self.blob_save(&version, &bytes)?;
                    Some(version)
                }
                None => None,
            };
            if let Some(existing) = record.changes.get(&relative) {
                ensure!(
                    existing.after == current,
                    "文件在本轮操作之间被其他来源修改：{}",
                    relative.display()
                );
            }
            record
                .changes
                .entry(relative.clone())
                .and_modify(|change| change.after = after.clone())
                .or_insert(Change {
                    before: current.clone(),
                    after: after.clone(),
                });
            updates.insert(
                relative,
                Change {
                    before: current,
                    after,
                },
            );
        }
        ensure!(
            record.changes.len() <= 1000,
            "本轮文件数量超过回撤备份上限（1000）"
        );
        let size: u64 = record
            .changes
            .values()
            .flat_map(|c| [&c.before, &c.after])
            .flatten()
            .map(|v| v.size)
            .sum();
        ensure!(size <= MAX_TURN, "本轮文件备份超过 128 MiB 上限");
        record.pending = true;
        self.save(&record)?;
        match self.transaction(&updates, || {
            record.pending = false;
            self.save(&record)
        }) {
            Ok(()) => Ok(()),
            Err(error) => {
                // Only clear pending if every original file has been recovered.
                if updates
                    .iter()
                    .all(|(path, c)| self.capture(path, false).ok().as_ref() == Some(&c.before))
                {
                    self.save(&previous)?;
                }
                Err(error)
            }
        }
    }

    pub fn preview(&self) -> Result<FileUndoPreview> {
        let mut preview = FileUndoPreview {
            turn_id: self.turn_id.clone(),
            workspace: self.workspace.display().to_string(),
            available: self.path().exists(),
            files: Vec::new(),
            conflicts: Vec::new(),
            warnings: Vec::new(),
            revision: String::new(),
        };
        if !preview.available {
            preview
                .warnings
                .push("这一轮没有文件恢复记录，只能撤回对话。".into());
            return Ok(preview);
        }
        let record = self.load()?;
        preview.warnings = record.warnings.clone();
        if record.pending {
            preview
                .conflicts
                .push("文件操作曾异常中断，自动恢复已暂停；原文件备份仍保留。".into());
        }
        if self.directory.join("restore-pending.json").exists() {
            preview
                .conflicts
                .push("上次回撤曾异常中断，请先核对文件；恢复前后的备份仍保留。".into());
        }
        let mut fingerprint = serde_json::to_vec(&record)?;
        for (path, change) in &record.changes {
            if change.before == change.after {
                continue;
            }
            preview.files.push(FileUndoEntry {
                path: path.to_string_lossy().into_owned(),
                action: if change.before.is_none() {
                    "delete"
                } else {
                    "restore"
                },
            });
            match self.capture(path, false) {
                Ok(current) => {
                    fingerprint.extend(serde_json::to_vec(&current)?);
                    if current != change.after {
                        preview
                            .conflicts
                            .push(format!("{}：本轮之后已变化", path.display()));
                    }
                }
                Err(error) => preview
                    .conflicts
                    .push(format!("{}：{error}", path.display())),
            }
            for version in [&change.before, &change.after].into_iter().flatten() {
                if let Err(error) = self.blob(version) {
                    preview
                        .conflicts
                        .push(format!("{}：{error}", path.display()));
                }
            }
        }
        preview.revision = digest(&fingerprint);
        Ok(preview)
    }

    /// The caller commits the conversation only after file restoration succeeds.
    /// If that durable commit fails, put all restored files back as well.
    pub fn restore_with(
        &self,
        revision: &str,
        commit: impl FnOnce() -> Result<()>,
    ) -> Result<usize> {
        let _guard = JOURNAL_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let preview = self.preview()?;
        ensure!(preview.available, "这一轮没有文件恢复记录");
        ensure!(
            preview.conflicts.is_empty(),
            "{}",
            preview.conflicts.join("；")
        );
        ensure!(preview.revision == revision, "恢复内容已变化，请重新预览");
        let record = self.load()?;
        let updates = record
            .changes
            .iter()
            .filter(|(_, c)| c.before != c.after)
            .map(|(path, c)| {
                (
                    path.clone(),
                    Change {
                        before: c.after.clone(),
                        after: c.before.clone(),
                    },
                )
            })
            .collect();
        // This transaction description plus immutable blobs survives interruption.
        atomic_write(
            &self.directory.join("restore-pending.json"),
            &serde_json::to_vec(&updates)?,
            None,
        )?;
        if let Err(error) = self.transaction(&updates, commit) {
            if updates
                .iter()
                .all(|(path, c)| self.capture(path, false).ok().as_ref() == Some(&c.before))
            {
                let _ = fs::remove_file(self.directory.join("restore-pending.json"));
            }
            return Err(error);
        }
        // Conversation already committed; failure to remove this audit marker
        // must not report a failed undo and invite a duplicate operation.
        let _ = fs::rename(
            self.directory.join("restore-pending.json"),
            self.directory
                .join(format!("restore-{}.json", Uuid::new_v4())),
        );
        Ok(preview.files.len())
    }

    fn transaction(
        &self,
        updates: &BTreeMap<PathBuf, Change>,
        commit: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        // Check every path and every backup before making the first change.
        for (path, change) in updates {
            ensure!(
                self.capture(path, false)? == change.before,
                "文件在确认后变化：{}",
                path.display()
            );
            for version in [&change.before, &change.after].into_iter().flatten() {
                self.blob(version)?;
            }
        }
        let mut applied = Vec::new();
        let result = (|| {
            for (path, change) in updates {
                ensure!(
                    self.capture(path, false)? == change.before,
                    "文件在恢复期间变化：{}",
                    path.display()
                );
                self.install(path, change.after.as_ref())?;
                applied.push(path);
            }
            commit()
        })();
        if let Err(error) = result {
            let mut failures = Vec::new();
            for path in applied.into_iter().rev() {
                let change = &updates[path];
                if self.capture(path, false).ok().as_ref() != Some(&change.after)
                    || self.install(path, change.before.as_ref()).is_err()
                {
                    failures.push(path.display().to_string());
                }
            }
            if !failures.is_empty() {
                bail!(
                    "{error}；部分文件未能恢复原状：{}。备份保存在 {}",
                    failures.join("、"),
                    self.directory.display()
                );
            }
            return Err(error.context("操作失败，已恢复操作前的文件"));
        }
        Ok(())
    }

    fn install(&self, relative: &Path, version: Option<&Version>) -> Result<()> {
        let path = self.workspace.join(relative);
        self.relative_path(&path)?;
        if let Some(version) = version {
            let bytes = self.blob(version)?;
            atomic_write(&path, &bytes, Some(version.mode))
        } else {
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("文件没有父目录")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".coomi-write-{}", Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn default_mode() -> u32 {
    0o644
}
fn file_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        default_mode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, FileJournal) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("workspace")).unwrap();
        fs::create_dir_all(dir.path().join("home")).unwrap();
        let journal = FileJournal::new(
            &dir.path().join("home"),
            &dir.path().join("workspace"),
            &Uuid::new_v4().to_string(),
            "turn1",
        )
        .unwrap();
        (dir, journal)
    }
    fn edit(journal: &FileJournal, name: &str, after: Option<&[u8]>) -> FileEdit {
        let path = journal.workspace.join(name);
        FileEdit {
            before: fs::read(&path).ok(),
            path,
            after: after.map(Vec::from),
            after_mode: None,
        }
    }

    #[test]
    fn undo_restores_exact_bytes_only_for_this_turn_and_survives_reopen() -> Result<()> {
        let (_dir, journal) = setup();
        fs::write(journal.workspace.join("before.txt"), b"user dirty\r\n")?;
        fs::write(journal.workspace.join("delete.bin"), [0, 255, 3, 0])?;
        journal.apply(vec![
            edit(&journal, "before.txt", Some(b"first")),
            edit(&journal, "new.txt", Some(b"new")),
            edit(&journal, "delete.bin", None),
        ])?;
        journal.apply(vec![edit(&journal, "before.txt", Some(b"second"))])?;
        fs::write(journal.workspace.join("unrelated.txt"), b"later user work")?;
        let reopened = FileJournal::new(
            &journal.home,
            &journal.workspace,
            journal
                .directory
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            "turn1",
        )?;
        let preview = reopened.preview()?;
        assert_eq!(preview.files.len(), 3);
        assert!(preview.conflicts.is_empty());
        reopened.restore_with(&preview.revision, || Ok(()))?;
        assert_eq!(
            fs::read(journal.workspace.join("before.txt"))?,
            b"user dirty\r\n"
        );
        assert_eq!(
            fs::read(journal.workspace.join("delete.bin"))?,
            [0, 255, 3, 0]
        );
        assert!(!journal.workspace.join("new.txt").exists());
        assert_eq!(
            fs::read(journal.workspace.join("unrelated.txt"))?,
            b"later user work"
        );
        Ok(())
    }

    #[test]
    fn sequential_turns_unwind_without_touching_another_file() -> Result<()> {
        let (_dir, first) = setup();
        let second = FileJournal {
            directory: first.directory.with_file_name("second"),
            turn_id: "second".into(),
            ..first.clone()
        };
        first.apply(vec![edit(&first, "a.txt", Some(b"one"))])?;
        second.apply(vec![edit(&second, "a.txt", Some(b"two"))])?;
        assert!(!first.preview()?.conflicts.is_empty());
        second.restore_with(&second.preview()?.revision, || Ok(()))?;
        assert_eq!(fs::read(first.workspace.join("a.txt"))?, b"one");
        first.restore_with(&first.preview()?.revision, || Ok(()))?;
        assert!(!first.workspace.join("a.txt").exists());
        Ok(())
    }

    #[test]
    fn changed_files_and_stale_previews_are_rejected_without_partial_undo() -> Result<()> {
        let (_dir, journal) = setup();
        journal.apply(vec![
            edit(&journal, "a", Some(b"a")),
            edit(&journal, "b", Some(b"b")),
        ])?;
        let preview = journal.preview()?;
        fs::write(journal.workspace.join("b"), b"other session")?;
        assert!(
            journal
                .restore_with(&preview.revision, || panic!("must not commit"))
                .is_err()
        );
        assert_eq!(fs::read(journal.workspace.join("a"))?, b"a");
        assert_eq!(fs::read(journal.workspace.join("b"))?, b"other session");
        assert!(
            journal
                .apply(vec![edit(&journal, "b", Some(b"would overwrite"))])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn failed_conversation_save_restores_files_and_allows_retry() -> Result<()> {
        let (_dir, journal) = setup();
        fs::write(journal.workspace.join("a"), b"before")?;
        journal.apply(vec![
            edit(&journal, "a", Some(b"after")),
            edit(&journal, "new", Some(b"new")),
        ])?;
        let preview = journal.preview()?;
        assert!(
            journal
                .restore_with(&preview.revision, || bail!("simulated disk error"))
                .is_err()
        );
        assert_eq!(fs::read(journal.workspace.join("a"))?, b"after");
        assert_eq!(fs::read(journal.workspace.join("new"))?, b"new");
        assert!(journal.preview()?.conflicts.is_empty());
        journal.restore_with(&preview.revision, || Ok(()))?;
        assert_eq!(fs::read(journal.workspace.join("a"))?, b"before");
        Ok(())
    }

    #[test]
    fn corrupt_backups_block_restoration() -> Result<()> {
        let (_dir, journal) = setup();
        fs::create_dir_all(journal.workspace.join("folder"))?;
        fs::write(journal.workspace.join("folder/a"), b"old")?;
        journal.apply(vec![edit(&journal, "folder/a", Some(b"new"))])?;
        let record = journal.load()?;
        let hash = &record.changes[Path::new("folder/a")]
            .before
            .as_ref()
            .unwrap()
            .hash;
        fs::write(journal.directory.join(hash), b"corrupt")?;
        assert!(!journal.preview()?.conflicts.is_empty());
        assert!(journal.restore_with("any", || Ok(())).is_err());
        assert_eq!(fs::read(journal.workspace.join("folder/a"))?, b"new");
        Ok(())
    }

    #[test]
    fn traversal_and_metadata_paths_are_rejected() -> Result<()> {
        let (_dir, journal) = setup();
        assert!(
            journal
                .relative_path(&journal.workspace.join("../outside"))
                .is_err()
        );
        assert!(
            journal
                .relative_path(&journal.workspace.join(".git/config"))
                .is_err()
        );
        assert!(
            journal
                .relative_path(&journal.home.join("settings"))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn interrupted_operations_keep_backups_and_block_new_restoration() -> Result<()> {
        let (_dir, journal) = setup();
        journal.apply(vec![edit(&journal, "file", Some(b"created"))])?;
        let mut record = journal.load()?;
        record.pending = true;
        journal.save(&record)?;
        assert!(!journal.preview()?.conflicts.is_empty());
        assert!(
            journal
                .restore_with("any", || panic!("must not commit"))
                .is_err()
        );
        record.pending = false;
        journal.save(&record)?;
        fs::write(journal.directory.join("restore-pending.json"), b"{}")?;
        assert!(!journal.preview()?.conflicts.is_empty());
        assert!(
            journal
                .apply(vec![edit(&journal, "file", Some(b"changed"))])
                .is_err()
        );
        assert_eq!(fs::read(journal.workspace.join("file"))?, b"created");
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn filesystem_failure_mid_restore_rolls_back_earlier_files() -> Result<()> {
        use std::os::windows::fs::OpenOptionsExt;
        let (_dir, journal) = setup();
        fs::write(journal.workspace.join("a"), b"original")?;
        journal.apply(vec![
            edit(&journal, "a", Some(b"modified")),
            edit(&journal, "b", Some(b"new")),
        ])?;
        let path = journal.workspace.join("b");
        // Permit reads/writes, but hold a real OS handle that denies deletion.
        let held_file = fs::OpenOptions::new()
            .read(true)
            .share_mode(3)
            .open(&path)?;
        let preview = journal.preview()?;
        let result =
            journal.restore_with(&preview.revision, || panic!("must not save conversation"));
        drop(held_file);
        assert!(result.is_err());
        assert_eq!(fs::read(journal.workspace.join("a"))?, b"modified");
        assert_eq!(fs::read(&path)?, b"new");
        assert!(journal.preview()?.conflicts.is_empty());
        journal.restore_with(&journal.preview()?.revision, || Ok(()))?;
        assert_eq!(fs::read(journal.workspace.join("a"))?, b"original");
        assert!(!path.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn replaced_parent_symlink_cannot_redirect_restore() -> Result<()> {
        let (dir, journal) = setup();
        fs::create_dir_all(journal.workspace.join("folder"))?;
        journal.apply(vec![edit(&journal, "folder/a", Some(b"new"))])?;
        fs::create_dir(dir.path().join("outside"))?;
        fs::write(dir.path().join("outside/a"), b"private")?;
        fs::rename(
            journal.workspace.join("folder"),
            journal.workspace.join("original"),
        )?;
        std::os::unix::fs::symlink(dir.path().join("outside"), journal.workspace.join("folder"))?;
        assert!(!journal.preview()?.conflicts.is_empty());
        assert!(journal.restore_with("any", || Ok(())).is_err());
        assert_eq!(fs::read(dir.path().join("outside/a"))?, b"private");
        Ok(())
    }
}
