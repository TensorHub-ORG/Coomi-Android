use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const TASK_SCHEMA_VERSION: u32 = 2;
pub const DEFAULT_MAX_CONCURRENT_TASKS: usize = 5;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    WaitingLock,
    Running,
    PausePending,
    Paused,
    AwaitingApproval,
    AwaitingInput,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Conflict,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::WaitingLock => "waiting_lock",
            Self::Running => "running",
            Self::PausePending => "pause_pending",
            Self::Paused => "paused",
            Self::AwaitingApproval => "awaiting_approval",
            Self::AwaitingInput => "awaiting_input",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Conflict => "conflict",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Conflict
        )
    }

    fn may_transition_to(self, next: Self) -> bool {
        use TaskStatus as S;
        matches!(
            (self, next),
            (S::Queued, S::WaitingLock | S::Running | S::Cancelled)
                | (
                    S::WaitingLock,
                    S::Running | S::PausePending | S::Cancelled | S::Interrupted | S::Conflict
                )
                | (
                    S::Running,
                    S::PausePending
                        | S::AwaitingApproval
                        | S::AwaitingInput
                        | S::Completed
                        | S::Failed
                        | S::Cancelled
                        | S::Interrupted
                        | S::Conflict
                )
                | (
                    S::PausePending,
                    S::Paused | S::Running | S::Cancelled | S::Interrupted
                )
                | (S::Paused, S::Queued | S::Cancelled)
                | (
                    S::AwaitingApproval,
                    S::Running | S::Cancelled | S::Interrupted
                )
                | (S::AwaitingInput, S::Running | S::Cancelled | S::Interrupted)
                | (
                    S::Failed | S::Cancelled | S::Interrupted | S::Conflict | S::Completed,
                    S::Queued
                )
        ) || self == next
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    #[default]
    Normal,
    High,
}

impl TaskPriority {
    fn score(self) -> u64 {
        match self {
            Self::Low => 0,
            Self::Normal => 10,
            Self::High => 20,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ResourceKey {
    pub kind: ResourceKind,
    pub identity: String,
}

impl ResourceKey {
    pub fn new(kind: ResourceKind, identity: impl Into<String>) -> Self {
        Self {
            kind,
            identity: normalize_identity(&identity.into()),
        }
    }

    pub fn file(path: &Path) -> Self {
        Self::new(ResourceKind::File, path.to_string_lossy())
    }

    pub fn git(path: &Path) -> Self {
        Self::new(ResourceKind::GitRepository, path.to_string_lossy())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Workspace,
    File,
    GitRepository,
    PackageManager,
    RuntimeInstall,
    Download,
    CognitiveRuntime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceAccess {
    Read,
    Write,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceRequest {
    pub key: ResourceKey,
    pub access: ResourceAccess,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConflictBaseline {
    #[serde(default)]
    pub file_sha256: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_index_sha256: Option<String>,
    #[serde(default)]
    pub lockfile_sha256: BTreeMap<String, Option<String>>,
}

impl ConflictBaseline {
    pub fn capture(workspace: &Path, files: &[PathBuf]) -> Result<Self> {
        let mut baseline = Self::default();
        for path in files {
            baseline.file_sha256.insert(
                path.to_string_lossy().into_owned(),
                digest_optional_file(path)?,
            );
        }
        baseline.git_head = git_output(workspace, &["rev-parse", "HEAD"]);
        baseline.git_index_sha256 = digest_optional_file(&workspace.join(".git/index"))?;
        for name in [
            "Cargo.lock",
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "gradle.lockfile",
        ] {
            let path = workspace.join(name);
            if path.exists() {
                baseline.lockfile_sha256.insert(
                    path.to_string_lossy().into_owned(),
                    digest_optional_file(&path)?,
                );
            }
        }
        Ok(baseline)
    }

    pub fn changed_paths(&self, workspace: &Path) -> Result<Vec<String>> {
        let mut changed = Vec::new();
        for (path, expected) in self.file_sha256.iter().chain(self.lockfile_sha256.iter()) {
            if &digest_optional_file(Path::new(path))? != expected {
                changed.push(path.clone());
            }
        }
        if let Some(expected) = &self.git_head
            && git_output(workspace, &["rev-parse", "HEAD"]).as_ref() != Some(expected)
        {
            changed.push("git:HEAD".into());
        }
        if let Some(expected) = &self.git_index_sha256
            && digest_optional_file(&workspace.join(".git/index"))?.as_ref() != Some(expected)
        {
            changed.push("git:index".into());
        }
        Ok(changed)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessLimits {
    pub runtime_seconds: u64,
    pub output_bytes: u64,
    pub memory_bytes: u64,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            runtime_seconds: 30 * 60,
            output_bytes: 16 * 1024 * 1024,
            memory_bytes: 512 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskRecord {
    pub version: u32,
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub status: TaskStatus,
    pub priority: TaskPriority,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default)]
    pub resources: Vec<ResourceRequest>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub lock_wait_ms: u64,
    #[serde(default)]
    pub baseline: Option<ConflictBaseline>,
    #[serde(default)]
    pub limits: ProcessLimits,
    #[serde(default)]
    pub resumable_stage: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskEvent {
    pub at_ms: u64,
    pub event: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Clone, Debug)]
struct HeldResource {
    task_id: String,
    access: ResourceAccess,
}

pub struct ResourceLease {
    task_id: String,
    keys: Vec<ResourceKey>,
    files: Vec<File>,
    held: Arc<Mutex<HashMap<ResourceKey, Vec<HeldResource>>>>,
    active: Arc<Mutex<HashSet<String>>>,
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        for file in &self.files {
            let _ = FileExt::unlock(file);
        }
        let mut held = self.held.lock().unwrap_or_else(|value| value.into_inner());
        for key in &self.keys {
            if let Some(entries) = held.get_mut(key) {
                entries.retain(|entry| entry.task_id != self.task_id);
                if entries.is_empty() {
                    held.remove(key);
                }
            }
        }
        self.active
            .lock()
            .unwrap_or_else(|value| value.into_inner())
            .remove(&self.task_id);
    }
}

#[derive(Clone)]
pub struct TaskManager {
    root: PathBuf,
    max_concurrent: usize,
    records: Arc<Mutex<BTreeMap<String, TaskRecord>>>,
    held: Arc<Mutex<HashMap<ResourceKey, Vec<HeldResource>>>>,
    active: Arc<Mutex<HashSet<String>>>,
}

impl TaskManager {
    pub fn open(home: &Path) -> Result<Self> {
        Self::open_with_limit(home, DEFAULT_MAX_CONCURRENT_TASKS)
    }

    pub fn open_with_limit(home: &Path, max_concurrent: usize) -> Result<Self> {
        anyhow::ensure!(
            max_concurrent > 0,
            "task concurrency limit must be positive"
        );
        let root = home.join("tasks");
        fs::create_dir_all(root.join("locks"))?;
        let manager = Self {
            root,
            max_concurrent,
            records: Arc::new(Mutex::new(BTreeMap::new())),
            held: Arc::new(Mutex::new(HashMap::new())),
            active: Arc::new(Mutex::new(HashSet::new())),
        };
        manager.load_records()?;
        manager.recover_interrupted()?;
        Ok(manager)
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    pub fn create(
        &self,
        session_id: impl Into<String>,
        kind: impl Into<String>,
        priority: TaskPriority,
        resources: Vec<ResourceRequest>,
    ) -> Result<TaskRecord> {
        let now = now_ms();
        let record = TaskRecord {
            version: TASK_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            session_id: session_id.into(),
            kind: kind.into(),
            status: TaskStatus::Queued,
            priority,
            created_at_ms: now,
            updated_at_ms: now,
            resources,
            model: None,
            skills: Vec::new(),
            retries: 0,
            error: None,
            lock_wait_ms: 0,
            baseline: None,
            limits: ProcessLimits::default(),
            resumable_stage: None,
        };
        self.records
            .lock()
            .unwrap_or_else(|value| value.into_inner())
            .insert(record.id.clone(), record.clone());
        self.persist_record(&record)?;
        self.append_event(&record.id, "created", record.status, None)?;
        Ok(record)
    }

    pub fn list(&self) -> Vec<TaskRecord> {
        let mut records = self
            .records
            .lock()
            .unwrap_or_else(|value| value.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| std::cmp::Reverse(record.updated_at_ms));
        records
    }

    pub fn get(&self, id: &str) -> Option<TaskRecord> {
        self.records
            .lock()
            .unwrap_or_else(|value| value.into_inner())
            .get(id)
            .cloned()
    }

    pub fn transition(
        &self,
        id: &str,
        next: TaskStatus,
        summary: Option<&str>,
    ) -> Result<TaskRecord> {
        let record = {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            let record = records
                .get_mut(id)
                .with_context(|| format!("task `{id}` was not found"))?;
            anyhow::ensure!(
                record.status.may_transition_to(next),
                "invalid task transition {:?} -> {:?}",
                record.status,
                next
            );
            record.status = next;
            record.updated_at_ms = now_ms();
            if matches!(next, TaskStatus::Failed | TaskStatus::Conflict) {
                record.error = summary.map(sanitize_log_text);
            } else if next == TaskStatus::Queued {
                record.error = None;
            }
            record.clone()
        };
        self.persist_record(&record)?;
        self.append_event(id, "status", next, summary)?;
        Ok(record)
    }

    pub fn request_pause(&self, id: &str) -> Result<TaskRecord> {
        let current = self
            .get(id)
            .with_context(|| format!("task `{id}` was not found"))?;
        match current.status {
            TaskStatus::Queued | TaskStatus::WaitingLock => {
                self.transition(id, TaskStatus::PausePending, Some("pause requested"))
            }
            TaskStatus::Running => self.transition(
                id,
                TaskStatus::PausePending,
                Some("pause will apply at the next safe point"),
            ),
            _ => anyhow::bail!("task cannot be paused from {:?}", current.status),
        }
    }

    pub fn reach_safe_point(&self, id: &str) -> Result<TaskRecord> {
        let current = self
            .get(id)
            .with_context(|| format!("task `{id}` was not found"))?;
        if current.status == TaskStatus::PausePending {
            self.transition(id, TaskStatus::Paused, Some("paused at safe point"))
        } else {
            Ok(current)
        }
    }

    pub fn resume(&self, id: &str) -> Result<TaskRecord> {
        let current = self
            .get(id)
            .with_context(|| format!("task `{id}` was not found"))?;
        match current.status {
            TaskStatus::Paused => self.transition(id, TaskStatus::Queued, Some("resume requested")),
            TaskStatus::PausePending => {
                self.transition(id, TaskStatus::Running, Some("pending pause withdrawn"))
            }
            _ => anyhow::bail!("task cannot be resumed from {:?}", current.status),
        }
    }

    pub fn retry(&self, id: &str) -> Result<TaskRecord> {
        let record = {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            let record = records
                .get_mut(id)
                .with_context(|| format!("task `{id}` was not found"))?;
            anyhow::ensure!(
                matches!(
                    record.status,
                    TaskStatus::Failed
                        | TaskStatus::Cancelled
                        | TaskStatus::Interrupted
                        | TaskStatus::Conflict
                        | TaskStatus::Completed
                ),
                "task cannot be retried from {:?}",
                record.status
            );
            record.status = TaskStatus::Queued;
            record.retries = record.retries.saturating_add(1);
            record.error = None;
            record.updated_at_ms = now_ms();
            record.clone()
        };
        self.persist_record(&record)?;
        self.append_event(id, "retry", TaskStatus::Queued, Some("retry confirmed"))?;
        Ok(record)
    }

    pub fn set_priority(&self, id: &str, priority: TaskPriority) -> Result<TaskRecord> {
        let record = self.update_record(id, |record| record.priority = priority)?;
        self.append_event(
            id,
            "priority",
            record.status,
            Some(match priority {
                TaskPriority::Low => "low",
                TaskPriority::Normal => "normal",
                TaskPriority::High => "high",
            }),
        )?;
        Ok(record)
    }

    pub fn set_context(
        &self,
        id: &str,
        model: Option<String>,
        skills: Vec<String>,
    ) -> Result<TaskRecord> {
        self.update_record(id, |record| {
            record.model = model;
            record.skills = skills;
        })
    }

    pub fn events(&self, id: &str) -> Result<Vec<TaskEvent>> {
        let path = self.task_dir(id).join("events.jsonl");
        let body = match fs::read_to_string(path) {
            Ok(body) => body,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        Ok(body
            .lines()
            .filter_map(|line| serde_json::from_str::<TaskEvent>(line).ok())
            .collect())
    }

    pub fn next_queued(&self) -> Option<TaskRecord> {
        let now = now_ms();
        self.records
            .lock()
            .unwrap_or_else(|value| value.into_inner())
            .values()
            .filter(|record| record.status == TaskStatus::Queued)
            .max_by_key(|record| {
                let age_bonus = now.saturating_sub(record.created_at_ms) / 60_000;
                (
                    record.priority.score() + age_bonus.min(40),
                    u64::MAX - record.created_at_ms,
                )
            })
            .cloned()
    }

    pub fn acquire(&self, task_id: &str) -> Result<Option<ResourceLease>> {
        let record = self
            .get(task_id)
            .with_context(|| format!("task `{task_id}` was not found"))?;
        let mut requests = record.resources.clone();
        requests.sort_by(|left, right| left.key.cmp(&right.key));
        {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            if !active.contains(task_id) && active.len() >= self.max_concurrent {
                return Ok(None);
            }
            active.insert(task_id.to_owned());
        }
        {
            let held = self.held.lock().unwrap_or_else(|value| value.into_inner());
            let blocked = requests.iter().any(|request| {
                held.get(&request.key).is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry.task_id != task_id
                            && (entry.access == ResourceAccess::Write
                                || request.access == ResourceAccess::Write)
                    })
                })
            });
            if blocked {
                self.active
                    .lock()
                    .unwrap_or_else(|value| value.into_inner())
                    .remove(task_id);
                return Ok(None);
            }
        }

        let mut files = Vec::with_capacity(requests.len());
        for request in &requests {
            let path = self.lock_path(&request.key);
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .with_context(|| format!("failed to open resource lock {}", path.display()))?;
            let acquired = match request.access {
                ResourceAccess::Read => FileExt::try_lock_shared(&file),
                ResourceAccess::Write => FileExt::try_lock_exclusive(&file),
            };
            if acquired.is_err() {
                for locked in &files {
                    let _ = FileExt::unlock(locked);
                }
                self.active
                    .lock()
                    .unwrap_or_else(|value| value.into_inner())
                    .remove(task_id);
                return Ok(None);
            }
            files.push(file);
        }
        let keys = requests
            .iter()
            .map(|request| request.key.clone())
            .collect::<Vec<_>>();
        let mut held = self.held.lock().unwrap_or_else(|value| value.into_inner());
        for request in requests {
            held.entry(request.key).or_default().push(HeldResource {
                task_id: task_id.to_owned(),
                access: request.access,
            });
        }
        drop(held);
        Ok(Some(ResourceLease {
            task_id: task_id.to_owned(),
            keys,
            files,
            held: Arc::clone(&self.held),
            active: Arc::clone(&self.active),
        }))
    }

    pub fn set_baseline(&self, id: &str, baseline: ConflictBaseline) -> Result<TaskRecord> {
        self.update_record(id, |record| record.baseline = Some(baseline))
    }

    pub fn verify_baseline(&self, id: &str, workspace: &Path) -> Result<Vec<String>> {
        let record = self
            .get(id)
            .with_context(|| format!("task `{id}` was not found"))?;
        record.baseline.as_ref().map_or_else(
            || Ok(Vec::new()),
            |baseline| baseline.changed_paths(workspace),
        )
    }

    pub fn append_output(&self, id: &str, bytes: &[u8]) -> Result<()> {
        let record = self
            .get(id)
            .with_context(|| format!("task `{id}` was not found"))?;
        let path = self.task_dir(id).join("output.log");
        let current = fs::metadata(&path).map(|value| value.len()).unwrap_or(0);
        anyhow::ensure!(
            current.saturating_add(bytes.len() as u64) <= record.limits.output_bytes,
            "task output exceeded {} bytes",
            record.limits.output_bytes
        );
        fs::create_dir_all(self.task_dir(id))?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?
            .write_all(bytes)?;
        Ok(())
    }

    fn update_record(&self, id: &str, update: impl FnOnce(&mut TaskRecord)) -> Result<TaskRecord> {
        let record = {
            let mut records = self
                .records
                .lock()
                .unwrap_or_else(|value| value.into_inner());
            let record = records
                .get_mut(id)
                .with_context(|| format!("task `{id}` was not found"))?;
            update(record);
            record.updated_at_ms = now_ms();
            record.clone()
        };
        self.persist_record(&record)?;
        Ok(record)
    }

    fn load_records(&self) -> Result<()> {
        let mut loaded = BTreeMap::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || entry.file_name() == "locks" {
                continue;
            }
            let path = entry.path().join("metadata.json");
            let backup = entry.path().join("metadata.json.bak");
            let bytes = fs::read(&path).or_else(|_| fs::read(&backup));
            let Ok(bytes) = bytes else { continue };
            let Ok(mut record) = serde_json::from_slice::<TaskRecord>(&bytes) else {
                continue;
            };
            record.version = TASK_SCHEMA_VERSION;
            loaded.insert(record.id.clone(), record);
        }
        *self
            .records
            .lock()
            .unwrap_or_else(|value| value.into_inner()) = loaded;
        Ok(())
    }

    fn recover_interrupted(&self) -> Result<()> {
        let ids = self
            .records
            .lock()
            .unwrap_or_else(|value| value.into_inner())
            .values()
            .filter(|record| {
                matches!(
                    record.status,
                    TaskStatus::Running
                        | TaskStatus::WaitingLock
                        | TaskStatus::PausePending
                        | TaskStatus::AwaitingApproval
                        | TaskStatus::AwaitingInput
                )
            })
            .map(|record| record.id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            self.transition(
                &id,
                TaskStatus::Interrupted,
                Some("engine restarted; explicit retry is required"),
            )?;
        }
        Ok(())
    }

    fn persist_record(&self, record: &TaskRecord) -> Result<()> {
        let directory = self.task_dir(&record.id);
        fs::create_dir_all(&directory)?;
        atomic_write_json(&directory.join("metadata.json"), record)
    }

    fn append_event(
        &self,
        id: &str,
        event: &str,
        status: TaskStatus,
        summary: Option<&str>,
    ) -> Result<()> {
        let directory = self.task_dir(id);
        fs::create_dir_all(&directory)?;
        let item = TaskEvent {
            at_ms: now_ms(),
            event: event.to_owned(),
            status,
            summary: summary.map(sanitize_log_text),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("events.jsonl"))?;
        serde_json::to_writer(&mut file, &item)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }

    fn task_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn lock_path(&self, key: &ResourceKey) -> PathBuf {
        let bytes = serde_json::to_vec(key).unwrap_or_else(|_| key.identity.as_bytes().to_vec());
        self.root
            .join("locks")
            .join(format!("{}.lock", sha256_bytes(&bytes)))
    }
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("metadata path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("json.tmp");
    let backup = path.with_extension("json.bak");
    {
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    if path.exists() {
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(path, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        if backup.exists() {
            let _ = fs::rename(&backup, path);
        }
        return Err(error.into());
    }
    if backup.exists() {
        fs::remove_file(backup)?;
    }
    Ok(())
}

fn digest_optional_file(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_bytes(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalize_identity(value: &str) -> String {
    value.replace('\\', "/").trim_end_matches('/').to_owned()
}

fn sanitize_log_text(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("api_key") || lower.contains("authorization:") || lower.contains("bearer ") {
        return "[sensitive task detail omitted]".into();
    }
    value.chars().take(512).collect()
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

    #[test]
    fn state_machine_pauses_only_at_safe_point_and_recovers() {
        let home = tempfile::tempdir().expect("temporary home");
        let manager = TaskManager::open(home.path()).expect("open manager");
        let record = manager
            .create("session", "agent", TaskPriority::Normal, Vec::new())
            .expect("create task");
        manager
            .transition(&record.id, TaskStatus::Running, None)
            .expect("start task");
        assert_eq!(
            manager
                .request_pause(&record.id)
                .expect("request pause")
                .status,
            TaskStatus::PausePending
        );
        assert_eq!(
            manager
                .reach_safe_point(&record.id)
                .expect("safe point")
                .status,
            TaskStatus::Paused
        );
        manager.resume(&record.id).expect("resume");
        manager
            .transition(&record.id, TaskStatus::Running, None)
            .expect("restart task");
        drop(manager);
        let reopened = TaskManager::open(home.path()).expect("reopen manager");
        assert_eq!(
            reopened.get(&record.id).expect("restored task").status,
            TaskStatus::Interrupted
        );
    }

    #[test]
    fn write_lock_blocks_readers_and_writers_for_same_resource() {
        let home = tempfile::tempdir().expect("temporary home");
        let manager = TaskManager::open(home.path()).expect("open manager");
        let key = ResourceKey::new(ResourceKind::Workspace, "/workspace");
        let writer = manager
            .create(
                "one",
                "agent",
                TaskPriority::Normal,
                vec![ResourceRequest {
                    key: key.clone(),
                    access: ResourceAccess::Write,
                }],
            )
            .expect("create writer");
        let reader = manager
            .create(
                "two",
                "agent",
                TaskPriority::Normal,
                vec![ResourceRequest {
                    key,
                    access: ResourceAccess::Read,
                }],
            )
            .expect("create reader");
        let lease = manager.acquire(&writer.id).expect("acquire writer");
        assert!(lease.is_some());
        assert!(manager.acquire(&reader.id).expect("try reader").is_none());
        drop(lease);
        assert!(
            manager
                .acquire(&reader.id)
                .expect("acquire reader")
                .is_some()
        );
    }

    #[test]
    fn manager_never_exceeds_configured_global_concurrency() {
        let home = tempfile::tempdir().expect("temporary home");
        let manager = TaskManager::open_with_limit(home.path(), 1).expect("open manager");
        let first = manager
            .create("one", "agent", TaskPriority::Normal, Vec::new())
            .expect("create first");
        let second = manager
            .create("two", "agent", TaskPriority::High, Vec::new())
            .expect("create second");
        let first_lease = manager
            .acquire(&first.id)
            .expect("acquire first")
            .expect("first lease");
        assert!(manager.acquire(&second.id).expect("try second").is_none());
        drop(first_lease);
        assert!(
            manager
                .acquire(&second.id)
                .expect("acquire second")
                .is_some()
        );
    }

    #[test]
    fn baseline_detects_external_file_and_git_index_changes() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let file = workspace.path().join("sample.txt");
        fs::write(&file, "before").expect("write baseline file");
        let baseline = ConflictBaseline::capture(workspace.path(), std::slice::from_ref(&file))
            .expect("capture baseline");
        fs::write(&file, "after").expect("change file");
        assert_eq!(
            baseline.changed_paths(workspace.path()).expect("compare"),
            vec![file.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn priority_ages_without_starving_low_priority_tasks() {
        let home = tempfile::tempdir().expect("temporary home");
        let manager = TaskManager::open(home.path()).expect("open manager");
        let low = manager
            .create("one", "agent", TaskPriority::Low, Vec::new())
            .expect("create low");
        manager
            .update_record(&low.id, |record| {
                record.created_at_ms = record.created_at_ms.saturating_sub(50 * 60_000)
            })
            .expect("age task");
        manager
            .create("two", "agent", TaskPriority::High, Vec::new())
            .expect("create high");
        assert_eq!(manager.next_queued().expect("next task").id, low.id);
    }
}
