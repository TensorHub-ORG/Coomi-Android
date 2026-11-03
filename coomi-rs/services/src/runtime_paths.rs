use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// The namespace in which a path is interpreted.  Host paths are used by
/// Android file APIs, while guest paths are visible inside ProotLinux.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathNamespace {
    Host,
    Termux,
    Guest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResolvedPath {
    pub namespace: PathNamespace,
    pub input: PathBuf,
    pub host_path: PathBuf,
    pub guest_path: PathBuf,
}

/// Runtime-owned mapping between Android/Termux storage and the Proot guest.
/// The guest aliases are stable; host roots are discovered at runtime.
#[derive(Clone, Debug)]
pub struct RuntimePathMap {
    host_workspace: PathBuf,
    host_runtime_home: Option<PathBuf>,
    host_build_root: Option<PathBuf>,
    host_tmp: Option<PathBuf>,
    termux_home: Option<PathBuf>,
    termux_prefix: Option<PathBuf>,
}

impl RuntimePathMap {
    pub fn new(host_workspace: impl Into<PathBuf>) -> Self {
        Self {
            host_workspace: host_workspace.into(),
            host_runtime_home: None,
            host_build_root: None,
            host_tmp: None,
            termux_home: None,
            termux_prefix: None,
        }
    }

    pub fn with_runtime_root(mut self, runtime_root: impl Into<PathBuf>) -> Self {
        let root = runtime_root.into();
        let home = root.join("home");
        self.host_runtime_home = Some(home.clone());
        self.host_build_root = Some(home.join(".coomi-dev"));
        self.host_tmp = Some(root.join("tmp"));
        self
    }

    pub fn with_termux(mut self, home: impl Into<PathBuf>, prefix: impl Into<PathBuf>) -> Self {
        self.termux_home = Some(home.into());
        self.termux_prefix = Some(prefix.into());
        self
    }

    pub fn host_workspace(&self) -> &Path {
        &self.host_workspace
    }
    pub fn host_runtime_home(&self) -> Option<&Path> {
        self.host_runtime_home.as_deref()
    }

    pub fn resolve(
        &self,
        value: impl AsRef<Path>,
        source: Option<PathNamespace>,
    ) -> Result<ResolvedPath> {
        let input = value.as_ref().to_path_buf();
        let namespace = source.unwrap_or_else(|| self.infer_namespace(&input));
        let host_path = match namespace {
            PathNamespace::Host => {
                if input.is_absolute() {
                    input.clone()
                } else {
                    self.host_workspace.join(&input)
                }
            }
            PathNamespace::Guest => self.guest_to_host(&input)?,
            PathNamespace::Termux => self.termux_to_host(&input)?,
        };
        let host_path = lexical_normalize(&host_path)?;
        let guest_path = self.host_to_guest(&host_path);
        Ok(ResolvedPath {
            namespace,
            input,
            host_path,
            guest_path,
        })
    }

    fn infer_namespace(&self, value: &Path) -> PathNamespace {
        let text = value.to_string_lossy();
        if text == "/workspace"
            || text.starts_with("/workspace/")
            || text == "/home/coomi"
            || text.starts_with("/home/coomi/")
            || text == "/opt/coomi-dev"
            || text.starts_with("/opt/coomi-dev/")
            || text == "/tmp"
            || text.starts_with("/tmp/")
        {
            PathNamespace::Guest
        } else {
            PathNamespace::Host
        }
    }

    fn guest_to_host(&self, value: &Path) -> Result<PathBuf> {
        let text = value.to_string_lossy();
        if text == "/workspace" || text.starts_with("/workspace/") {
            return Ok(self.host_workspace.join(
                text.strip_prefix("/workspace")
                    .unwrap_or_default()
                    .trim_start_matches('/'),
            ));
        }
        if text == "/home/coomi" || text.starts_with("/home/coomi/") {
            let root = self
                .host_runtime_home
                .as_ref()
                .ok_or_else(|| anyhow!("Proot runtime home is not configured"))?;
            return Ok(root.join(
                text.strip_prefix("/home/coomi")
                    .unwrap_or_default()
                    .trim_start_matches('/'),
            ));
        }
        if text == "/opt/coomi-dev" || text.starts_with("/opt/coomi-dev/") {
            let root = self
                .host_build_root
                .as_ref()
                .ok_or_else(|| anyhow!("Proot build kit is not configured"))?;
            return Ok(root.join(
                text.strip_prefix("/opt/coomi-dev")
                    .unwrap_or_default()
                    .trim_start_matches('/'),
            ));
        }
        if text == "/tmp" || text.starts_with("/tmp/") {
            let root = self
                .host_tmp
                .as_ref()
                .ok_or_else(|| anyhow!("Proot temporary directory is not configured"))?;
            return Ok(root.join(
                text.strip_prefix("/tmp")
                    .unwrap_or_default()
                    .trim_start_matches('/'),
            ));
        }
        Err(anyhow!(
            "unsupported guest path {}; use /workspace, /home/coomi, /opt/coomi-dev, or /tmp",
            value.display()
        ))
    }

    fn termux_to_host(&self, value: &Path) -> Result<PathBuf> {
        if value.is_absolute() {
            return Ok(value.to_path_buf());
        }
        self.termux_home
            .as_ref()
            .map(|home| home.join(value))
            .ok_or_else(|| anyhow!("Termux home is not configured"))
    }

    pub fn host_to_guest(&self, value: &Path) -> PathBuf {
        if let Ok(relative) = value.strip_prefix(&self.host_workspace) {
            return join_guest("/workspace", relative);
        }
        if let Some(root) = &self.host_build_root {
            if let Ok(relative) = value.strip_prefix(root) {
                return join_guest("/opt/coomi-dev", relative);
            }
        }
        if let Some(root) = &self.host_runtime_home {
            if let Ok(relative) = value.strip_prefix(root) {
                return join_guest("/home/coomi", relative);
            }
        }
        if let Some(root) = &self.host_tmp {
            if let Ok(relative) = value.strip_prefix(root) {
                return join_guest("/tmp", relative);
            }
        }
        value.to_path_buf()
    }
}

fn join_guest(root: &str, relative: &Path) -> PathBuf {
    let mut path = PathBuf::from(root);
    if !relative.as_os_str().is_empty() {
        path.push(relative);
    }
    path
}

fn lexical_normalize(path: &Path) -> Result<PathBuf> {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    return Err(anyhow!("path escapes its root"));
                }
            }
            other => output.push(other.as_os_str()),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_guest_aliases_to_host_and_back() {
        let map = RuntimePathMap::new("/app/workspace").with_runtime_root("/app/runtime-v2");
        let resolved = map
            .resolve("/workspace/src/main.rs", None)
            .expect("guest path");
        assert_eq!(
            resolved.host_path,
            PathBuf::from("/app/workspace/src/main.rs")
        );
        assert_eq!(resolved.guest_path, PathBuf::from("/workspace/src/main.rs"));
        let home = map
            .resolve("/home/coomi/custom_coomi", None)
            .expect("guest home");
        assert_eq!(
            home.host_path,
            PathBuf::from("/app/runtime-v2/home/custom_coomi")
        );
    }

    #[test]
    fn rejects_unknown_guest_alias() {
        let map = RuntimePathMap::new("/app/workspace");
        assert!(
            map.resolve("/usr/bin/python", Some(PathNamespace::Guest))
                .is_err()
        );
    }
}
