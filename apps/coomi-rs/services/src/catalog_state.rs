use anyhow::Context;
use anyhow::Result;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledSkill {
    pub name: String,
    pub enabled: bool,
    pub path: PathBuf,
    pub source: String,
    pub source_type: String,
    pub commit: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredMcp {
    pub name: String,
    pub transport: String,
    pub enabled: bool,
    pub target: String,
}

pub fn list_installed_skills(home: &Path) -> Result<Vec<InstalledSkill>> {
    let path = home.join("config").join("skills.json");
    let document = read_json_or_default(&path, json!({"version": 1, "skills": {}}))?;
    let configured = document
        .get("skills")
        .and_then(Value::as_object)
        .context("skills.json has no skills object")?;
    let mut names = BTreeSet::new();
    names.extend(configured.keys().cloned());
    if let Ok(entries) = fs::read_dir(home.join("skills")) {
        names.extend(
            entries
                .flatten()
                .filter(|entry| entry.path().join("SKILL.md").is_file())
                .map(|entry| entry.file_name().to_string_lossy().into_owned()),
        );
    }
    Ok(names
        .into_iter()
        .filter_map(|name| {
            let record = configured.get(&name).and_then(Value::as_object);
            let path = record
                .and_then(|record| record.get("path"))
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("skills").join(&name));
            path.join("SKILL.md").is_file().then(|| InstalledSkill {
                name,
                enabled: record
                    .and_then(|record| record.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                path,
                source: record
                    .and_then(|record| record.get("source"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                source_type: record
                    .and_then(|record| record.get("source_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("untracked")
                    .to_owned(),
                commit: record
                    .and_then(|record| record.get("commit"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect())
}

pub fn set_skill_enabled(home: &Path, name: &str, enabled: bool) -> Result<()> {
    validate_name(name)?;
    let path = home.join("config").join("skills.json");
    let mut document = read_json_or_default(&path, json!({"version": 1, "skills": {}}))?;
    let skills = document
        .get_mut("skills")
        .and_then(Value::as_object_mut)
        .context("skills.json has no skills object")?;
    let record = skills.entry(name).or_insert_with(|| {
        json!({
            "path": home.join("skills").join(name),
            "source": "",
            "source_type": "untracked"
        })
    });
    record
        .as_object_mut()
        .context("Skill record must be an object")?
        .insert("enabled".into(), Value::Bool(enabled));
    save_json(&path, &document)
}

pub fn remove_installed_skill(home: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    let config_path = home.join("config").join("skills.json");
    let mut document = read_json_or_default(&config_path, json!({"version": 1, "skills": {}}))?;
    let skills = document
        .get_mut("skills")
        .and_then(Value::as_object_mut)
        .context("skills.json has no skills object")?;
    let configured_path = skills
        .get(name)
        .and_then(Value::as_object)
        .and_then(|record| record.get("path"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let target = configured_path.unwrap_or_else(|| home.join("skills").join(name));
    let root = home.join("skills");
    fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    if target.exists() {
        let target = target.canonicalize()?;
        anyhow::ensure!(
            target != root && target.starts_with(&root),
            "refusing to remove a Skill outside {}",
            root.display()
        );
        fs::remove_dir_all(&target)
            .with_context(|| format!("failed to remove Skill {}", target.display()))?;
    }
    skills.remove(name);
    save_json(&config_path, &document)
}

pub fn list_configured_mcp(home: &Path) -> Result<Vec<ConfiguredMcp>> {
    let path = home.join("config").join("mcp_servers.json");
    let document = read_json_or_default(&path, json!({"version": 1, "servers": {}}))?;
    let servers = document
        .get("servers")
        .and_then(Value::as_object)
        .context("mcp_servers.json has no servers object")?;
    Ok(servers
        .iter()
        .map(|(name, value)| {
            let transport = value
                .get("transport")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    if value.get("command").is_some() {
                        "stdio"
                    } else {
                        "http"
                    }
                })
                .to_owned();
            let target = if transport == "stdio" {
                value
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            } else {
                value.get("url").and_then(Value::as_str).unwrap_or_default()
            };
            ConfiguredMcp {
                name: name.clone(),
                transport,
                enabled: value
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                target: target.to_owned(),
            }
        })
        .collect())
}

pub fn set_mcp_enabled(home: &Path, name: &str, enabled: bool) -> Result<()> {
    update_mcp_document(home, |servers| {
        servers
            .get_mut(name)
            .with_context(|| format!("MCP server `{name}` is not configured"))?
            .as_object_mut()
            .context("MCP server record must be an object")?
            .insert("enabled".into(), Value::Bool(enabled));
        Ok(())
    })
}

pub fn remove_configured_mcp(home: &Path, name: &str) -> Result<()> {
    update_mcp_document(home, |servers| {
        anyhow::ensure!(
            servers.remove(name).is_some(),
            "MCP server `{name}` is not configured"
        );
        Ok(())
    })
}

/// 读取单个 MCP server 的原始配置（未掩码）。
pub fn get_mcp_config(home: &Path, name: &str) -> Result<Option<Value>> {
    let path = home.join("config").join("mcp_servers.json");
    let document = read_json_or_default(&path, json!({"version": 1, "servers": {}}))?;
    let servers = document
        .get("servers")
        .and_then(Value::as_object)
        .context("mcp_servers.json has no servers object")?;
    Ok(servers.get(name).cloned())
}

/// 添加或更新 MCP server 配置（原子写）。返回 true 表示新建，false 表示覆盖已有。
pub fn upsert_mcp_config(home: &Path, name: &str, config: &Value) -> Result<bool> {
    validate_name(name)?;
    anyhow::ensure!(config.is_object(), "MCP config must be an object");
    let transport = config
        .get("transport")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if config.get("command").is_some() {
                "stdio"
            } else {
                "http"
            }
        });
    anyhow::ensure!(
        matches!(transport, "stdio" | "http" | "sse"),
        "unsupported MCP transport"
    );
    if transport == "stdio" {
        anyhow::ensure!(
            config
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty()),
            "stdio MCP command is required"
        );
    } else {
        anyhow::ensure!(
            config.get("url").and_then(Value::as_str).is_some_and(|value| {
                value.starts_with("http://") || value.starts_with("https://")
            }),
            "remote MCP URL must use http or https"
        );
    }
    let existed = std::cell::Cell::new(false);
    update_mcp_document(home, |servers| {
        existed.set(servers.contains_key(name));
        servers.insert(name.to_owned(), config.clone());
        Ok(())
    })?;
    Ok(!existed.get())
}

fn update_mcp_document(
    home: &Path,
    update: impl FnOnce(&mut serde_json::Map<String, Value>) -> Result<()>,
) -> Result<()> {
    let path = home.join("config").join("mcp_servers.json");
    let mut document = read_json_or_default(&path, json!({"version": 1, "servers": {}}))?;
    let servers = document
        .get_mut("servers")
        .and_then(Value::as_object_mut)
        .context("mcp_servers.json has no servers object")?;
    update(servers)?;
    save_json(&path, &document)
}

fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '.'))
            && Path::new(name).components().count() == 1
            && !matches!(name, "." | ".."),
        "name must be one directory component"
    );
    Ok(())
}

fn read_json_or_default(path: &Path, default: Value) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid configuration {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn save_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // 先写临时文件再替换，避免写到一半被打断留下半截 JSON。
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("failed to write configuration {}", temporary.display()))?;
    #[cfg(windows)]
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to save configuration {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_and_disables_untracked_skills() {
        let home = tempfile::tempdir().expect("temporary home");
        let skill = home.path().join("skills").join("review");
        fs::create_dir_all(&skill).expect("create Skill");
        fs::write(skill.join("SKILL.md"), "# Review").expect("write Skill");
        assert!(list_installed_skills(home.path()).expect("list")[0].enabled);
        set_skill_enabled(home.path(), "review", false).expect("disable");
        assert!(!list_installed_skills(home.path()).expect("list")[0].enabled);
    }

    #[test]
    fn toggles_and_removes_mcp_records() {
        let home = tempfile::tempdir().expect("temporary home");
        let path = home.path().join("config").join("mcp_servers.json");
        fs::create_dir_all(path.parent().expect("config parent")).expect("create config");
        fs::write(
            &path,
            r#"{"servers":{"docs":{"transport":"http","url":"https://example.test"}}}"#,
        )
        .expect("write MCP config");
        set_mcp_enabled(home.path(), "docs", false).expect("disable");
        assert!(!list_configured_mcp(home.path()).expect("list")[0].enabled);
        remove_configured_mcp(home.path(), "docs").expect("remove");
        assert!(list_configured_mcp(home.path()).expect("list").is_empty());
    }

    #[test]
    fn upserts_and_reads_mcp_config() {
        let home = tempfile::tempdir().expect("temporary home");
        let config = json!({
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "server"],
            "env": {"TOKEN": "secret"}
        });
        assert!(upsert_mcp_config(home.path(), "demo", &config).expect("create"));
        assert!(!upsert_mcp_config(home.path(), "demo", &config).expect("update"));
        let stored = get_mcp_config(home.path(), "demo").expect("read").expect("present");
        assert_eq!(stored["command"], "npx");
        assert_eq!(stored["env"]["TOKEN"], "secret");

        // 名称必须能安全落盘；远端 transport 只接受 http(s)。
        assert!(upsert_mcp_config(home.path(), "../escape", &config).is_err());
        assert!(upsert_mcp_config(
            home.path(),
            "remote",
            &json!({"transport": "http", "url": "ftp://example.test"})
        )
        .is_err());

        // 省略 transport 时按 command / url 推断。
        assert!(upsert_mcp_config(
            home.path(),
            "inferred",
            &json!({"command": "node", "args": ["server.js"]})
        )
        .is_ok());
    }
}
