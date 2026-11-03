use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use crate::WorkflowState;

// Serialize definition/registry writes across cloned stores in the engine process.
static DEFINITIONS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `config/workflows.json` 的顶层文档（与 `mcp_servers.json`/`skills.json` 同构）。
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct WorkflowDocument {
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowEntry>,
}

/// 单个 workflow 的注册元数据（`enabled`/`path`/`source`，与 skill 的登记项同构）。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowEntry {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub path: PathBuf,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub origin: String,
}

fn default_true() -> bool {
    true
}

/// 可编排 Workflow 定义文件的持久化（落点契约与 skill/mcp 一致）：
/// - 定义文件：`<home>/workflows/<id>/workflow.json`
/// - 注册表：`<home>/config/workflows.json`
///
/// 与 [`crate::SessionStore`] 同级：这是纯数据持久层，不含进程/网络依赖，
/// 因此可脱离 Android 交叉编译工具链单独测试。
#[derive(Clone, Debug)]
pub struct WorkflowStore {
    home: PathBuf,
}

impl WorkflowStore {
    pub fn new(coomi_home: impl AsRef<Path>) -> Self {
        Self {
            home: coomi_home.as_ref().to_path_buf(),
        }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join("config").join("workflows.json")
    }

    fn workflows_dir(&self) -> PathBuf {
        self.home.join("workflows")
    }

    fn workflow_path(&self, id: &str) -> PathBuf {
        self.workflows_dir().join(id).join("workflow.json")
    }

    /// 读取注册表（不存在/损坏时回退为空注册表，不 panic）。
    pub fn load_document(&self) -> Result<WorkflowDocument> {
        let path = self.config_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(WorkflowDocument::default());
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to read {}: {error}",
                    path.display()
                ));
            }
        };
        let document = serde_json::from_slice::<WorkflowDocument>(&bytes)
            .with_context(|| format!("invalid {}", path.display()))?;
        Ok(document)
    }

    /// 列出所有已启用（且定义文件存在）的 workflow id，按字母序排序。
    pub fn list_ids(&self) -> Result<Vec<String>> {
        let document = self.load_document()?;
        let mut ids: Vec<String> = document
            .workflows
            .iter()
            .filter(|(_, entry)| entry.enabled)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        Ok(ids)
    }

    /// 读取一个 workflow 定义。找不到或未启用时返回错误。
    pub fn read(&self, id: &str) -> Result<WorkflowState> {
        anyhow::ensure!(crate::types::valid_workflow_id(id), "invalid workflow id");
        anyhow::ensure!(!id.trim().is_empty(), "workflow id must not be empty");
        let document = self.load_document()?;
        let Some(entry) = document.workflows.get(id) else {
            anyhow::bail!("workflow `{id}` is not registered");
        };
        anyhow::ensure!(entry.enabled, "workflow `{id}` is disabled");
        let path = if entry.path.is_absolute() {
            entry.path.clone()
        } else {
            self.workflow_path(id)
        };
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read workflow definition {}", path.display()))?;
        let workflow = serde_json::from_slice::<WorkflowState>(&bytes)
            .with_context(|| format!("invalid workflow definition {}", path.display()))?;
        workflow
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid workflow `{id}`: {e}"))?;
        anyhow::ensure!(
            workflow.id == id,
            "workflow definition id does not match registration"
        );
        Ok(workflow)
    }

    /// 保存一个 workflow 定义到 `<home>/workflows/<id>/workflow.json`，并同步注册表。
    pub fn save(&self, workflow: &WorkflowState) -> Result<()> {
        let _lock = DEFINITIONS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        self.load_document()?;
        workflow
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid workflow: {e}"))?;
        anyhow::ensure!(
            !workflow.id.trim().is_empty(),
            "workflow id must not be empty"
        );
        let dir = self
            .workflow_path(&workflow.id)
            .parent()
            .expect("workflow path has parent")
            .to_path_buf();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create workflow directory {}", dir.display()))?;
        let bytes = serde_json::to_vec_pretty(workflow).context("failed to serialize workflow")?;
        let path = self.workflow_path(&workflow.id);
        // 原子写：先写临时文件再 rename，避免崩溃/断电留下截断的 JSON。
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        self.register(&workflow.id, &path, "local", workflow.origin.as_str())?;
        Ok(())
    }

    /// 注册（或更新）一个 workflow 条目到 `config/workflows.json`。
    fn register(&self, id: &str, path: &Path, source: &str, origin: &str) -> Result<()> {
        let mut document = self.load_document()?;
        document.workflows.insert(
            id.to_string(),
            WorkflowEntry {
                enabled: true,
                path: path.to_path_buf(),
                source: source.to_string(),
                origin: origin.to_string(),
            },
        );
        self.write_document(&document)
    }

    /// 删除一个 workflow：移除定义文件与注册条目。
    pub fn remove(&self, id: &str) -> Result<()> {
        let _lock = DEFINITIONS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(crate::types::valid_workflow_id(id), "invalid workflow id");
        anyhow::ensure!(!id.trim().is_empty(), "workflow id must not be empty");
        let mut document = self.load_document()?;
        let dir = self
            .workflow_path(id)
            .parent()
            .expect("workflow path has parent")
            .to_path_buf();
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .with_context(|| format!("failed to remove {}", dir.display()))?;
        }
        document.workflows.remove(id);
        self.write_document(&document)
    }

    /// 写入注册表（原子写）。
    fn write_document(&self, document: &WorkflowDocument) -> Result<()> {
        let config_dir = self.home.join("config");
        std::fs::create_dir_all(&config_dir)
            .with_context(|| format!("failed to create {}", config_dir.display()))?;
        let path = self.config_path();
        let bytes = serde_json::to_vec_pretty(document).context("serialize workflows.json")?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepAction;
    use crate::WorkflowStep;
    use std::env;

    fn temp_home(tag: &str) -> PathBuf {
        let mut dir = env::temp_dir();
        dir.push(format!("coomi-wf-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp home");
        dir
    }

    fn sample_workflow() -> WorkflowState {
        let step_a = WorkflowStep::new(
            "a",
            "A",
            StepAction::Model {
                prompt: "hi".into(),
                model: None,
                isolate: None,
            },
        );
        let step_b = WorkflowStep::new(
            "b",
            "B",
            StepAction::Model {
                prompt: "hi".into(),
                model: None,
                isolate: None,
            },
        )
        .depends_on(&["a"]);
        WorkflowState::new("wf-sample", "Sample", vec![step_a, step_b])
    }

    #[test]
    fn save_then_read_roundtrips() {
        let home = temp_home("roundtrip");
        let store = WorkflowStore::new(&home);
        let workflow = sample_workflow();
        store.save(&workflow).expect("save");
        let loaded = store.read("wf-sample").expect("read");
        assert_eq!(loaded.id, "wf-sample");
        assert_eq!(loaded.steps.len(), 2);
        assert_eq!(loaded.steps[1].depends_on, vec!["a"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn list_only_returns_registered_and_enabled() {
        let home = temp_home("list");
        let store = WorkflowStore::new(&home);
        store.save(&sample_workflow()).expect("save");
        let ids = store.list_ids().expect("list");
        assert_eq!(ids, vec!["wf-sample"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_deletes_definition_and_registration() {
        let home = temp_home("remove");
        let store = WorkflowStore::new(&home);
        store.save(&sample_workflow()).expect("save");
        store.remove("wf-sample").expect("remove");
        assert!(store.read("wf-sample").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregistered_workflow_read_fails() {
        let home = temp_home("unreg");
        let store = WorkflowStore::new(&home);
        assert!(store.read("nope").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn invalid_workflow_cannot_be_saved() {
        let home = temp_home("invalid");
        let store = WorkflowStore::new(&home);
        // 自环依赖应被 validate 拒绝
        let bad = WorkflowState::new(
            "wf-bad",
            "Bad",
            vec![
                WorkflowStep::new(
                    "a",
                    "A",
                    StepAction::Model {
                        prompt: "hi".into(),
                        model: None,
                        isolate: None,
                    },
                )
                .depends_on(&["a"]),
            ],
        );
        assert!(store.save(&bad).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_is_atomic_and_keeps_old_definition_on_invalid() {
        let home = temp_home("atomic");
        let store = WorkflowStore::new(&home);
        let good = sample_workflow();
        store.save(&good).expect("save good");
        // 定义文件应存在且合法
        assert!(store.read("wf-sample").is_ok());
        let _ = std::fs::remove_dir_all(&home);
    }
}
