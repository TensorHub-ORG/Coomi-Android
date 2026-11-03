use anyhow::Context;
use anyhow::Result;
use coomi_telemetry::Telemetry;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

const MCP_CATALOG: &str = include_str!("../mcp.json");
const SKILL_CATALOG: &str = include_str!("../skills.json");
const CUSTOM_ITERATION_SKILL: &str = include_str!("../coomi-custom-iteration.md");
const RUNTIME_ENVIRONMENT_SKILL: &str = include_str!("../runtime-environments.md");
const COOMIDEV_ENV: &str = include_str!("../../../../tools/mobile-build/coomidev-env.sh");
const COOMIDEV_DOCTOR: &str = include_str!("../../../../tools/mobile-build/coomidev-doctor.sh");
const COOMIDEV_BUILD: &str = include_str!("../../../../tools/mobile-build/build-coomidev.sh");
const COOMIDEV_INSTALL_BUILDKIT: &str =
    include_str!("../../../../tools/mobile-build/install-coomidev-buildkit.sh");

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Catalog<T> {
    pub version: u32,
    pub entries: Vec<T>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequiredParameter {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub required_parameters: Vec<RequiredParameter>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub repository: String,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub subdir: String,
}

pub fn builtin_mcp() -> Result<Catalog<McpEntry>> {
    serde_json::from_str(MCP_CATALOG).context("built-in MCP catalog is invalid")
}

pub fn builtin_skills() -> Result<Catalog<SkillEntry>> {
    serde_json::from_str(SKILL_CATALOG).context("built-in Skill catalog is invalid")
}

pub struct CatalogInstaller {
    home: PathBuf,
}

impl CatalogInstaller {
    pub fn new(home: impl AsRef<Path>) -> Self {
        Self {
            home: home.as_ref().to_path_buf(),
        }
    }

    pub fn install_mcp(&self, id: &str, values: &BTreeMap<String, String>) -> Result<PathBuf> {
        let catalog = builtin_mcp()?;
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id))
            .with_context(|| format!("MCP `{id}` is not in the built-in catalog"))?;
        for parameter in &entry.required_parameters {
            if values
                .get(&parameter.key)
                .is_none_or(|value| value.trim().is_empty())
            {
                anyhow::bail!(
                    "缺少必填参数 `{}` ({})，请填写后再安装",
                    parameter.key,
                    parameter.label
                )
            }
        }

        let args = entry
            .args
            .iter()
            .map(|value| substitute(value, values))
            .collect::<Result<Vec<_>>>()?;
        let env = entry
            .env
            .iter()
            .map(|(key, value)| Ok((key.clone(), substitute(value, values)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let path = self.home.join("config").join("mcp_servers.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut document = if path.exists() {
            serde_json::from_slice::<Value>(&fs::read(&path)?)
                .with_context(|| format!("invalid MCP config {}", path.display()))?
        } else {
            json!({"version": 1, "servers": {}})
        };
        let servers = document
            .get_mut("servers")
            .and_then(Value::as_object_mut)
            .context("MCP config must contain an object named `servers`")?;
        servers.insert(
            entry.id.clone(),
            json!({
                "transport": entry.transport,
                "command": entry.command,
                "args": args,
                "env": env,
                "enabled": true
            }),
        );
        fs::write(&path, serde_json::to_vec_pretty(&document)?)
            .with_context(|| format!("failed to write MCP config {}", path.display()))?;
        Ok(path)
    }

    pub fn install_skill(&self, id: &str) -> Result<PathBuf> {
        self.install_skill_inner(id, false)
    }

    /// Install the Coomi development workflow locally without a network call.
    /// This is intentionally bundled with the bridge so the entry-point flow
    /// remains usable before GitHub/gh has been configured.
    pub fn install_custom_iteration_skill(&self) -> Result<PathBuf> {
        let destination = self.home.join("skills").join("coomi-custom-iteration");
        fs::create_dir_all(&destination)?;
        fs::write(destination.join("SKILL.md"), CUSTOM_ITERATION_SKILL)?;
        save_skill_metadata(
            &self.home,
            &SkillEntry {
                id: "coomi-custom-iteration".into(),
                name: "Coomi Custom Iteration".into(),
                description: "Safely develop Coomi, submit PRs, or build CoomiDev.".into(),
                repository: "TensorHub-ORG/Coomi-Android".into(),
                git_ref: "main".into(),
                subdir: "".into(),
            },
            &destination,
            "bundled",
        )?;
        Ok(destination)
    }

    /// Install the bundled runtime coordination guidance. This is local and
    /// idempotent so a fresh runtime can route Host/Termux/Proot work before
    /// any network-backed Skill installation is available.
    pub fn install_runtime_environment_skill(&self) -> Result<PathBuf> {
        let destination = self.home.join("skills").join("runtime-environments");
        fs::create_dir_all(&destination)?;
        fs::write(destination.join("SKILL.md"), RUNTIME_ENVIRONMENT_SKILL)?;
        save_skill_metadata(
            &self.home,
            &SkillEntry {
                id: "runtime-environments".into(),
                name: "Runtime Environments".into(),
                description: "Route tools across Host, Termux, and ProotLinux without mixing paths or binaries.".into(),
                repository: "TensorHub-ORG/Coomi-Android".into(),
                git_ref: "main".into(),
                subdir: "".into(),
            },
            &destination,
            "bundled",
        )?;
        Ok(destination)
    }

    /// Install Coomi-owned Build Kit helpers into the persistent Runtime V2
    /// home. Compiler artifacts are deliberately not bundled here: the doctor
    /// must report not-ready until a pinned, checksum-verified kit is selected.
    pub fn install_custom_iteration_buildkit(&self) -> Result<PathBuf> {
        let root = self.home.join("runtime-v2").join("home").join(".coomi-dev");
        for directory in ["bin", "toolchains", "cache", "state", "logs", "keys"] {
            fs::create_dir_all(root.join(directory))?;
        }
        write_executable(&root.join("bin/coomidev-env"), COOMIDEV_ENV)?;
        write_executable(&root.join("bin/coomidev-doctor"), COOMIDEV_DOCTOR)?;
        write_executable(&root.join("bin/coomidev-build"), COOMIDEV_BUILD)?;
        write_executable(
            &root.join("bin/coomidev-install-buildkit"),
            COOMIDEV_INSTALL_BUILDKIT,
        )?;
        Ok(root)
    }

    pub fn update_skill(&self, id: &str) -> Result<PathBuf> {
        self.install_skill_inner(id, true)
    }

    /// 安装社区注册表条目（市场）：条目来自远端 registry.json，不经内置 catalog
    /// 查找，直接按 repository/ref/subdir 走与内置目录相同的下载安装流程。
    pub fn install_remote_skill(&self, entry: &SkillEntry, replace: bool) -> Result<PathBuf> {
        self.install_entry(entry, replace)
    }

    /// 卸载 Skill：删除 skills/{id} 目录与 config/skills.json 中的条目。
    pub fn uninstall_skill(&self, id: &str) -> Result<PathBuf> {
        // 与安装一致：id 必须先在内置目录中解析出合法条目，杜绝路径穿越
        // （id=".."、"%2E%2E%2F" 等经 URL 解码后越界删除任意目录）。
        let catalog = builtin_skills()?;
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id))
            .with_context(|| format!("Skill `{id}` is not in the built-in catalog"))?;
        let destination = self.home.join("skills").join(&entry.id);
        if destination.exists() {
            fs::remove_dir_all(&destination)
                .with_context(|| format!("failed to remove {}", destination.display()))?;
        }
        let config_path = self.home.join("config").join("skills.json");
        if config_path.exists() {
            let bytes = fs::read(&config_path)
                .with_context(|| format!("failed to read {}", config_path.display()))?;
            if let Ok(mut document) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(skills) = document.get_mut("skills").and_then(Value::as_object_mut) {
                    skills.remove(id);
                    fs::write(&config_path, serde_json::to_vec_pretty(&document)?)
                        .with_context(|| format!("failed to write {}", config_path.display()))?;
                }
            }
        }
        Ok(destination)
    }

    /// 从本地 zip 导入 Skill：解压后取含 SKILL.md 的目录（或整个 zip 根）复制到 skills/{id}，
    /// 并写入 config/skills.json。支持 zip-slip 防护。id 由调用方提供（前端从文件名生成）。
    pub fn import_skill_zip(&self, id: &str, zip_bytes: &[u8]) -> Result<PathBuf> {
        anyhow::ensure!(!id.trim().is_empty(), "Skill id is required");
        let safe_id = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect::<String>();
        let destination = self.home.join("skills").join(&safe_id);
        if destination.exists() {
            anyhow::bail!("Skill `{safe_id}` is already installed");
        }
        let cache = self.home.join("cache").join(format!("skill-import-{safe_id}"));
        if cache.exists() { fs::remove_dir_all(&cache)?; }
        fs::create_dir_all(&cache)?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .context("Skill zip is not a valid zip")?;
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).context("invalid zip entry")?;
            let name = file.name().to_string();
            let rest = name.trim_start_matches("./");
            if rest.split('/').any(|seg| seg.is_empty() || seg == "..") { continue; }
            let target = cache.join(rest);
            if file.is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() { fs::create_dir_all(parent)?; }
                let mut output = std::fs::File::create(&target)
                    .with_context(|| format!("failed to write {}", target.display()))?;
                std::io::copy(&mut file, &mut output)?;
            }
        }
        // 若 zip 根只有一个目录且含 SKILL.md，则用该子目录；否则用整个解压根。
        let source = find_skill_root(&cache)?;
        if !source.join("SKILL.md").is_file() {
            anyhow::bail!("导入的 Skill 包缺少 SKILL.md");
        }
        fs::create_dir_all(&destination)?;
        if let Err(error) = copy_directory(&source, &destination) {
            let _ = fs::remove_dir_all(&destination);
            return Err(error);
        }
        let entry = SkillEntry {
            id: safe_id.clone(),
            name: safe_id.clone(),
            description: String::new(),
            repository: "local-import".into(),
            git_ref: "local".into(),
            subdir: String::new(),
        };
        save_skill_metadata(&self.home, &entry, &destination, "local")?;
        let _ = fs::remove_dir_all(&cache);
        Ok(destination)
    }

    /// 把本地已存在的 skill 目录（含 SKILL.md）注册到 skills.json（无网络、无压缩包）。
    pub fn register_skill_directory(&self, id: &str, dir: &Path) -> Result<PathBuf> {
        anyhow::ensure!(dir.join("SKILL.md").is_file(), "directory has no SKILL.md");
        let destination = self.home.join("skills").join(id);
        if destination.exists() {
            anyhow::bail!("Skill `{id}` is already installed");
        }
        fs::create_dir_all(&destination)?;
        copy_directory(dir, &destination)?;
        let entry = SkillEntry {
            id: id.into(),
            name: id.into(),
            description: String::new(),
            repository: "local-import".into(),
            git_ref: "local".into(),
            subdir: String::new(),
        };
        save_skill_metadata(&self.home, &entry, &destination, "local")?;
        Ok(destination)
    }

    /// 直接写入/追加 MCP server 配置（自定义 MCP 导入）。
    pub fn import_mcp_config(&self, name: &str, config: &serde_json::Value) -> Result<PathBuf> {
        anyhow::ensure!(!name.trim().is_empty(), "MCP name is required");
        anyhow::ensure!(config.is_object(), "MCP config must be an object");
        let path = self.home.join("config").join("mcp_servers.json");
        let mut document = if path.exists() {
            serde_json::from_slice::<Value>(&fs::read(&path)?)
                .with_context(|| format!("invalid MCP config {}", path.display()))?
        } else {
            json!({"servers": {}})
        };
        let servers = document
            .get_mut("servers")
            .and_then(Value::as_object_mut)
            .context("mcp_servers.json has no servers object")?;
        let mut server = config.clone();
        if server.get("enabled").is_none() { server["enabled"] = json!(true); }
        servers.insert(name.trim().to_owned(), server);
        fs::write(&path, serde_json::to_vec_pretty(&document)?)?;
        Ok(path)
    }

    fn install_skill_inner(&self, id: &str, replace: bool) -> Result<PathBuf> {
        let catalog = builtin_skills()?;
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id.eq_ignore_ascii_case(id))
            .with_context(|| format!("Skill `{id}` is not in the built-in catalog"))?;
        self.install_entry(entry, replace)
    }

    /// 安装核心：下载 zip → 解压 subdir → 复制到 skills/{id} → 写元数据。
    /// 内置目录与社区市场共用此函数，埋点也集中在这里——无论用户手动点击
    /// 还是 Agent 自动安装，install_ok / install_fail 都在此处产生。
    fn install_entry(&self, entry: &SkillEntry, replace: bool) -> Result<PathBuf> {
        let result = self.install_entry_inner(entry, replace);
        let telemetry = Telemetry::new(&self.home);
        match &result {
            Ok(_) => {
                let _ = telemetry.record("install_ok", &entry.id);
            }
            Err(_) => {
                let _ = telemetry.record("install_fail", &entry.id);
            }
        }
        result
    }

    fn install_entry_inner(&self, entry: &SkillEntry, replace: bool) -> Result<PathBuf> {
        let destination = self.home.join("skills").join(&entry.id);
        if destination.exists() && !replace {
            anyhow::bail!("Skill `{}` is already installed", entry.id)
        }
        let cache = self
            .home
            .join("cache")
            .join(format!("skill-{}-partial", entry.id));
        if cache.exists() {
            fs::remove_dir_all(&cache)
                .with_context(|| format!("failed to clear cache {}", cache.display()))?;
        }
        if let Some(parent) = cache.parent() {
            fs::create_dir_all(parent)?;
        }
        // 通过 GitHub codeload zip 下载并解压（不依赖 git 命令：手机 bootstrap 未内置 git）。
        let zip_url = format!(
            "https://codeload.github.com/{}/zip/refs/heads/{}",
            entry.repository, entry.git_ref
        );
        let bytes = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .user_agent("coomi-android")
            .build()
            .context("failed to build download client")?
            .get(&zip_url)
            .send()
            .context("failed to download skill archive")?
            .error_for_status()
            .context("skill archive download failed")?
            .bytes()
            .context("failed to read skill archive")?;
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
            .context("skill archive is not a valid zip")?;
        // codeload zip 的根目录形如 {repo}-{ref}/，把其中 {subdir}/ 的内容解压到目标。
        let repo_basename = entry.repository.rsplit('/').next().unwrap_or("repository");
        // GitHub normalizes slashes in branch names in codeload archive roots
        // (for example codex/feat/x becomes repo-codex-feat-x).
        let root_prefixes = [
            format!("{repo_basename}-{}/", entry.git_ref),
            format!("{repo_basename}-{}/", entry.git_ref.replace('/', "-")),
        ];
        for index in 0..archive.len() {
            let mut file = archive
                .by_index(index)
                .with_context(|| format!("invalid zip entry #{index}"))?;
            let name = file.name().to_string();
            let Some(rest) = root_prefixes
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix))
            else {
                continue;
            };
            if !matches_skill_subdir(rest, &entry.subdir) {
                continue;
            }
            // zip-slip 防护：拒绝任何越界片段。
            if rest
                .split('/')
                .any(|segment| segment.is_empty() || segment == "..")
            {
                continue;
            }
            let target = cache.join(rest);
            if file.is_dir() {
                fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut output = std::fs::File::create(&target)
                    .with_context(|| format!("failed to write {}", target.display()))?;
                std::io::copy(&mut file, &mut output)?;
            }
        }
        let source = cache.join(&entry.subdir);
        if !source.is_dir() {
            anyhow::bail!("downloaded repository has no directory `{}`", entry.subdir)
        }
        // zip 包不带 commit hash，用 ref 名作为版本记录。
        let commit = entry.git_ref.clone();
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let backup = self
            .home
            .join("cache")
            .join(format!("skill-{}-backup", entry.id));
        if replace && destination.exists() {
            if backup.exists() {
                fs::remove_dir_all(&backup)?;
            }
            fs::rename(&destination, &backup)?;
        }
        if let Err(error) = copy_directory(&source, &destination) {
            let _ = fs::remove_dir_all(&destination);
            if backup.exists() {
                let _ = fs::rename(&backup, &destination);
            }
            return Err(error);
        }
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        save_skill_metadata(&self.home, entry, &destination, &commit)?;
        fs::remove_dir_all(&cache)
            .with_context(|| format!("failed to clear cache {}", cache.display()))?;
        Ok(destination)
    }
}

fn save_skill_metadata(
    home: &Path,
    entry: &SkillEntry,
    destination: &Path,
    commit: &str,
) -> Result<()> {
    let path = home.join("config").join("skills.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut document = if path.exists() {
        serde_json::from_slice::<Value>(&fs::read(&path)?)
            .with_context(|| format!("invalid Skill config {}", path.display()))?
    } else {
        json!({"version": 1, "skills": {}})
    };
    let skills = document
        .get_mut("skills")
        .and_then(Value::as_object_mut)
        .context("Skill config must contain an object named `skills`")?;
    skills.insert(
        entry.id.clone(),
        json!({
            "enabled": true,
            "path": destination,
            "source": entry.id,
            "source_type": "catalog",
            "repository": entry.repository,
            "git_ref": entry.git_ref,
            "subdir": entry.subdir,
            "commit": commit
        }),
    );
    fs::write(&path, serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

fn substitute(template: &str, values: &BTreeMap<String, String>) -> Result<String> {
    let mut output = template.to_string();
    while let Some(start) = output.find("{{") {
        let relative_end = output[start + 2..]
            .find("}}")
            .context("unclosed catalog placeholder")?;
        let end = start + 2 + relative_end;
        let key = &output[start + 2..end];
        let value = values
            .get(key)
            .with_context(|| format!("missing catalog parameter `{key}`"))?;
        output.replace_range(start..end + 2, value);
    }
    Ok(output)
}

/// 找 zip 解压后实际含 SKILL.md 的根目录：若唯一子目录含 SKILL.md 则用该子目录，否则用根。
fn find_skill_root(cache: &Path) -> Result<PathBuf> {
    let entries = fs::read_dir(cache)?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    if entries.len() == 1 && entries[0].path().is_dir() && entries[0].path().join("SKILL.md").is_file() {
        return Ok(entries[0].path());
    }
    Ok(cache.to_path_buf())
}

fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn write_executable(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_catalogs_are_valid_and_non_empty() {
        let mcp = builtin_mcp().expect("MCP catalog");
        assert!(!mcp.entries.is_empty());
        assert!(!builtin_skills().expect("Skill catalog").entries.is_empty());
        // Fetch 由引擎内置工具提供，不再出现在 MCP 安装目录中。
        assert!(mcp.entries.iter().all(|entry| entry.id != "fetch"));
    }

    #[test]
    fn installs_parameterized_mcp_config() {
        let home = tempfile::tempdir().expect("temporary home");
        let values = BTreeMap::from([(
            "allowed_path".to_string(),
            home.path().display().to_string(),
        )]);
        let path = CatalogInstaller::new(home.path())
            .install_mcp("filesystem", &values)
            .expect("install MCP");
        let document: Value = serde_json::from_slice(&fs::read(path).expect("read MCP config"))
            .expect("parse MCP config");
        assert_eq!(
            document.pointer("/servers/filesystem/enabled"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn installs_custom_iteration_buildkit_helpers_without_claiming_ready() {
        let home = tempfile::tempdir().expect("temporary home");
        let root = CatalogInstaller::new(home.path())
            .install_custom_iteration_buildkit()
            .expect("install Build Kit helpers");
        for helper in [
            "coomidev-env",
            "coomidev-doctor",
            "coomidev-build",
            "coomidev-install-buildkit",
        ] {
            assert!(root.join("bin").join(helper).is_file(), "missing {helper}");
        }
        assert!(root.join("toolchains").is_dir());
        assert!(!root.join("current/buildkit.json").exists());
    }

    #[test]
    fn uninstall_skill_rejects_path_traversal_ids() {
        // 卸载只接受内置目录中的合法 id：路径穿越（..、绝对路径、任意目录名）一律拒绝。
        let home = tempfile::tempdir().expect("temporary home");
        let installer = CatalogInstaller::new(home.path());
        for malicious in ["..", "../x", "/etc", "a/b", "%2e%2e"] {
            assert!(
                installer.uninstall_skill(malicious).is_err(),
                "uninstall should reject {malicious}"
            );
        }
        // 不存在的合法目录 id 不会报错（视为已卸载），但也不得删到 skills 之外。
        assert!(installer.uninstall_skill("frontend-design").is_ok());
        assert!(home.path().join("skills").is_dir() || !home.path().join("skills").exists());
    }

    #[test]
    fn root_subdir_accepts_skill_files_at_repository_root() {
        assert!(matches_skill_subdir("shizuku-skill-main/SKILL.md", "."));
        assert!(matches_skill_subdir(
            "shizuku-skill-main/agents/openai.yaml",
            "."
        ));
        assert!(!matches_skill_subdir(
            "shizuku-skill-main/other/SKILL.md",
            "agents"
        ));
    }
}

fn matches_skill_subdir(path: &str, subdir: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if subdir == "." {
        return true;
    }
    path == subdir || path.starts_with(&format!("{subdir}/"))
}
