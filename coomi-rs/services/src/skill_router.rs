use crate::list_installed_skills;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SKILL_INDEX_VERSION: u32 = 1;
const MAX_SKILL_FILE_BYTES: u64 = 128 * 1024;
const MAX_SKILL_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_REFERENCE_DEPTH: usize = 3;

#[derive(Clone, Debug, Default, Deserialize)]
struct SkillFrontMatter {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, deserialize_with = "string_or_list")]
    keywords: Vec<String>,
    #[serde(default, alias = "file-types", deserialize_with = "string_or_list")]
    file_types: Vec<String>,
    #[serde(default, alias = "tools", deserialize_with = "string_or_list")]
    tool_requirements: Vec<String>,
    #[serde(default, alias = "projects", deserialize_with = "string_or_list")]
    project_types: Vec<String>,
    #[serde(default, deserialize_with = "string_or_list")]
    risks: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillIndexEntry {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub enabled: bool,
    pub keywords: Vec<String>,
    pub file_types: Vec<String>,
    pub tool_requirements: Vec<String>,
    pub project_types: Vec<String>,
    pub risks: Vec<String>,
    pub indexed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillRouteStatus {
    Discovered,
    Read,
    Used,
    Skipped,
    Conflict,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkillRouteDecision {
    pub name: String,
    pub status: SkillRouteStatus,
    pub score: i64,
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SkillRouteContext {
    pub attachments: Vec<PathBuf>,
    pub expected_tools: Vec<String>,
    pub project_types: Vec<String>,
    pub network_allowed: bool,
    pub destructive_allowed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SkillIndexDocument {
    version: u32,
    entries: Vec<SkillIndexEntry>,
}

#[derive(Clone, Debug)]
pub struct SkillRouteResult {
    pub instructions: String,
    pub decisions: Vec<SkillRouteDecision>,
}

#[derive(Clone, Debug)]
pub struct SkillRouter {
    home: PathBuf,
    root: PathBuf,
    entries: Vec<SkillIndexEntry>,
}

impl SkillRouter {
    pub fn load(home: &Path) -> Result<Self> {
        let root = home.join("skills");
        fs::create_dir_all(&root)?;
        let mut router = Self {
            home: home.to_owned(),
            root,
            entries: Vec::new(),
        };
        router.reindex()?;
        Ok(router)
    }

    pub fn entries(&self) -> &[SkillIndexEntry] {
        &self.entries
    }

    pub fn reindex(&mut self) -> Result<()> {
        let allowed_root = canonical_or_self(&self.root)?;
        let mut entries = Vec::new();
        for installed in list_installed_skills(&self.home)? {
            let skill_file = installed.path.join("SKILL.md");
            let canonical = canonical_or_self(&skill_file)?;
            if !canonical.starts_with(&allowed_root) {
                entries.push(SkillIndexEntry {
                    name: installed.name,
                    description: String::new(),
                    path: skill_file,
                    enabled: false,
                    keywords: Vec::new(),
                    file_types: Vec::new(),
                    tool_requirements: Vec::new(),
                    project_types: Vec::new(),
                    risks: Vec::new(),
                    indexed_at_ms: now_ms(),
                    index_error: Some("Skill path is outside the allowed skills directory".into()),
                });
                continue;
            }
            match index_skill(&installed.name, &skill_file, installed.enabled) {
                Ok(entry) => entries.push(entry),
                Err(error) => entries.push(SkillIndexEntry {
                    name: installed.name,
                    description: String::new(),
                    path: skill_file,
                    enabled: false,
                    keywords: Vec::new(),
                    file_types: Vec::new(),
                    tool_requirements: Vec::new(),
                    project_types: Vec::new(),
                    risks: Vec::new(),
                    indexed_at_ms: now_ms(),
                    index_error: Some(format!("{error:#}")),
                }),
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        self.entries = entries;
        self.save_index()
    }

    pub fn route(&self, text: &str, context: &SkillRouteContext) -> Result<SkillRouteResult> {
        let query = tokenize(text);
        let extensions = context
            .attachments
            .iter()
            .filter_map(|path| path.extension().and_then(|value| value.to_str()))
            .map(normalize_tag)
            .collect::<HashSet<_>>();
        let expected_tools = context
            .expected_tools
            .iter()
            .map(|value| normalize_tag(value))
            .collect::<HashSet<_>>();
        let project_types = context
            .project_types
            .iter()
            .map(|value| normalize_tag(value))
            .collect::<HashSet<_>>();

        let mut candidates = self
            .entries
            .iter()
            .map(|entry| score_entry(entry, &query, &extensions, &expected_tools, &project_types))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(entry, score, _)| (Reverse(*score), entry.name.clone()));

        let mut instructions = String::new();
        let mut decisions = Vec::new();
        let mut used = 0usize;
        for (entry, score, reasons) in candidates {
            if !entry.enabled || entry.index_error.is_some() {
                decisions.push(SkillRouteDecision {
                    name: entry.name.clone(),
                    status: SkillRouteStatus::Skipped,
                    score,
                    reasons,
                    conflict: entry.index_error.clone(),
                });
                continue;
            }
            if score <= 0 || used >= 3 {
                decisions.push(SkillRouteDecision {
                    name: entry.name.clone(),
                    status: if score > 0 {
                        SkillRouteStatus::Discovered
                    } else {
                        SkillRouteStatus::Skipped
                    },
                    score,
                    reasons,
                    conflict: None,
                });
                continue;
            }
            if let Some(conflict) = route_conflict(entry, context) {
                decisions.push(SkillRouteDecision {
                    name: entry.name.clone(),
                    status: SkillRouteStatus::Conflict,
                    score,
                    reasons,
                    conflict: Some(conflict),
                });
                continue;
            }
            let body = match read_skill_tree(&entry.path, &self.root, 0, &mut BTreeSet::new()) {
                Ok(body) => body,
                Err(error) => {
                    decisions.push(SkillRouteDecision {
                        name: entry.name.clone(),
                        status: SkillRouteStatus::Skipped,
                        score,
                        reasons,
                        conflict: Some(format!("Skill could not be loaded: {error:#}")),
                    });
                    continue;
                }
            };
            let section = format!(
                "\n\n## Routed Skill: {}\nMatch: {}\n{}",
                entry.name,
                reasons.join(", "),
                body.trim()
            );
            if instructions.len().saturating_add(section.len()) > MAX_SKILL_CONTEXT_BYTES {
                decisions.push(SkillRouteDecision {
                    name: entry.name.clone(),
                    status: SkillRouteStatus::Skipped,
                    score,
                    reasons,
                    conflict: Some("selected Skill would exceed the routing context limit".into()),
                });
                continue;
            }
            instructions.push_str(&section);
            decisions.push(SkillRouteDecision {
                name: entry.name.clone(),
                status: SkillRouteStatus::Used,
                score,
                reasons,
                conflict: None,
            });
            used += 1;
        }
        self.write_audit(text, &decisions)?;
        Ok(SkillRouteResult {
            instructions,
            decisions,
        })
    }

    fn save_index(&self) -> Result<()> {
        let path = self.home.join("config").join("skill_index.json");
        let parent = path.parent().context("Skill index path has no parent")?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&SkillIndexDocument {
                version: SKILL_INDEX_VERSION,
                entries: self.entries.clone(),
            })?,
        )?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn write_audit(&self, text: &str, decisions: &[SkillRouteDecision]) -> Result<()> {
        let directory = self.home.join("skill-routing");
        fs::create_dir_all(&directory)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("events.jsonl"))?;
        let digest = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({
                "at_ms": now_ms(),
                "request_sha256": digest,
                "request_chars": text.chars().count(),
                "decisions": decisions,
            }),
        )?;
        file.write_all(b"\n")?;
        Ok(())
    }
}

fn index_skill(name: &str, path: &Path, enabled: bool) -> Result<SkillIndexEntry> {
    let metadata = fs::metadata(path)?;
    anyhow::ensure!(
        metadata.len() <= MAX_SKILL_FILE_BYTES,
        "SKILL.md exceeds {} bytes",
        MAX_SKILL_FILE_BYTES
    );
    let body = fs::read_to_string(path)
        .with_context(|| format!("failed to read Skill {}", path.display()))?;
    let (front_matter, markdown) = split_front_matter(&body)?;
    let heading = markdown
        .lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .unwrap_or(name);
    let description = if front_matter.description.trim().is_empty() {
        markdown
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect()
    } else {
        front_matter.description
    };
    let display_name = if front_matter.name.trim().is_empty() {
        heading.to_owned()
    } else {
        front_matter.name
    };
    let mut keywords = front_matter.keywords;
    keywords.extend(tokenize(&display_name));
    keywords.extend(tokenize(&description));
    normalize_list(&mut keywords);
    let mut file_types = front_matter.file_types;
    normalize_list(&mut file_types);
    let mut tool_requirements = front_matter.tool_requirements;
    normalize_list(&mut tool_requirements);
    let mut project_types = front_matter.project_types;
    normalize_list(&mut project_types);
    let mut risks = front_matter.risks;
    normalize_list(&mut risks);
    Ok(SkillIndexEntry {
        name: display_name,
        description,
        path: path.to_owned(),
        enabled,
        keywords,
        file_types,
        tool_requirements,
        project_types,
        risks,
        indexed_at_ms: now_ms(),
        index_error: None,
    })
}

fn split_front_matter(body: &str) -> Result<(SkillFrontMatter, &str)> {
    let Some(rest) = body
        .strip_prefix("---\n")
        .or_else(|| body.strip_prefix("---\r\n"))
    else {
        return Ok((SkillFrontMatter::default(), body));
    };
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim() == "---" {
            let yaml = &rest[..offset];
            let markdown = &rest[offset + line.len()..];
            return Ok((serde_yaml::from_str(yaml)?, markdown));
        }
        offset += line.len();
    }
    anyhow::bail!("unterminated SKILL.md front matter")
}

fn read_skill_tree(
    path: &Path,
    allowed_root: &Path,
    depth: usize,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<String> {
    anyhow::ensure!(
        depth <= MAX_REFERENCE_DEPTH,
        "Skill reference depth exceeded"
    );
    let allowed_root = canonical_or_self(allowed_root)?;
    let canonical = path.canonicalize()?;
    anyhow::ensure!(
        canonical.starts_with(&allowed_root),
        "Skill reference escapes allowed path"
    );
    anyhow::ensure!(
        visited.insert(canonical.clone()),
        "Skill reference cycle detected"
    );
    let metadata = fs::metadata(&canonical)?;
    anyhow::ensure!(
        metadata.len() <= MAX_SKILL_FILE_BYTES,
        "Skill reference is too large"
    );
    let body = fs::read_to_string(&canonical)?;
    let mut output = String::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(reference) = trimmed.strip_prefix("@include ") {
            let parent = canonical.parent().context("Skill path has no parent")?;
            let included = parent.join(reference.trim());
            let nested = read_skill_tree(&included, &allowed_root, depth + 1, visited)?;
            output.push_str(&nested);
            output.push('\n');
        } else {
            output.push_str(line);
            output.push('\n');
        }
        anyhow::ensure!(
            output.len() <= MAX_SKILL_CONTEXT_BYTES,
            "expanded Skill exceeds context limit"
        );
    }
    visited.remove(&canonical);
    Ok(output)
}

fn score_entry<'a>(
    entry: &'a SkillIndexEntry,
    query: &HashSet<String>,
    extensions: &HashSet<String>,
    expected_tools: &HashSet<String>,
    project_types: &HashSet<String>,
) -> (&'a SkillIndexEntry, i64, Vec<String>) {
    let mut score = 0i64;
    let mut reasons = Vec::new();
    let keyword_hits = entry
        .keywords
        .iter()
        .filter(|keyword| query.contains(keyword.as_str()))
        .count();
    if keyword_hits > 0 {
        score += (keyword_hits as i64) * 10;
        reasons.push(format!("{keyword_hits} keyword match(es)"));
    }
    let extension_hits = entry
        .file_types
        .iter()
        .filter(|extension| extensions.contains(extension.as_str()))
        .count();
    if extension_hits > 0 {
        score += (extension_hits as i64) * 14;
        reasons.push(format!("{extension_hits} file type match(es)"));
    }
    let tool_hits = entry
        .tool_requirements
        .iter()
        .filter(|tool| expected_tools.contains(tool.as_str()))
        .count();
    if tool_hits > 0 {
        score += (tool_hits as i64) * 8;
        reasons.push(format!("{tool_hits} tool match(es)"));
    }
    let project_hits = entry
        .project_types
        .iter()
        .filter(|project| project_types.contains(project.as_str()))
        .count();
    if project_hits > 0 {
        score += (project_hits as i64) * 6;
        reasons.push(format!("{project_hits} project match(es)"));
    }
    (entry, score, reasons)
}

fn route_conflict(entry: &SkillIndexEntry, context: &SkillRouteContext) -> Option<String> {
    if !context.network_allowed && entry.risks.iter().any(|risk| risk == "network") {
        return Some("project or user rules prohibit the Skill's network requirement".into());
    }
    if !context.destructive_allowed && entry.risks.iter().any(|risk| risk == "destructive") {
        return Some("project or user rules prohibit the Skill's destructive operations".into());
    }
    None
}

fn tokenize(value: &str) -> HashSet<String> {
    value
        .split(|character: char| {
            !(character.is_alphanumeric() || matches!(character, '_' | '-' | '+' | '.' | '#'))
        })
        .map(normalize_tag)
        .filter(|token| token.chars().count() >= 2)
        .collect()
}

fn normalize_tag(value: &str) -> String {
    value.trim().trim_start_matches('.').to_ascii_lowercase()
}

fn normalize_list(values: &mut Vec<String>) {
    *values = values
        .drain(..)
        .map(|value| normalize_tag(&value))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
}

fn canonical_or_self(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        Ok(path.canonicalize()?)
    } else {
        Ok(path.to_owned())
    }
}

fn string_or_list<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        One(String),
        Many(Vec<String>),
    }
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::One(value)) => value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(Value::Many(values)) => values,
        None => Vec::new(),
    })
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
    use serde_json::json;

    fn install_skill(home: &Path, name: &str, body: &str, enabled: bool) {
        let directory = home.join("skills").join(name);
        fs::create_dir_all(&directory).expect("create skill");
        fs::write(directory.join("SKILL.md"), body).expect("write Skill");
        let config = home.join("config").join("skills.json");
        fs::create_dir_all(config.parent().expect("config parent")).expect("create config");
        let mut document = if config.exists() {
            serde_json::from_slice::<serde_json::Value>(&fs::read(&config).expect("read config"))
                .expect("parse config")
        } else {
            json!({"version":1,"skills":{}})
        };
        document["skills"][name] = json!({
            "path": directory,
            "enabled": enabled,
            "source_type": "test"
        });
        fs::write(
            config,
            serde_json::to_vec(&document).expect("serialize config"),
        )
        .expect("write config");
    }

    #[test]
    fn oversized_skill_does_not_abort_or_hide_a_valid_match() {
        let home = tempfile::tempdir().unwrap();
        install_skill(
            home.path(),
            "a-large",
            &format!(
                "---\nkeywords: [review]\n---\n{}",
                "x".repeat(MAX_SKILL_CONTEXT_BYTES + 1)
            ),
            true,
        );
        install_skill(
            home.path(),
            "b-valid",
            "---\nkeywords: [review]\n---\nKeep the useful instruction.",
            true,
        );
        let result = SkillRouter::load(home.path())
            .unwrap()
            .route("review", &SkillRouteContext::default())
            .unwrap();
        assert!(result.instructions.contains("Keep the useful instruction."));
        assert!(result.decisions.iter().any(|d| {
            d.status == SkillRouteStatus::Skipped
                && d.conflict
                    .as_deref()
                    .is_some_and(|c| c.contains("expanded Skill exceeds"))
        }));
    }

    #[test]
    fn ranks_keyword_file_tool_and_project_matches() {
        let home = tempfile::tempdir().expect("temporary home");
        install_skill(
            home.path(),
            "rust-review",
            "---\nname: Rust review\nkeywords: [review, rust]\nfile_types: [rs]\ntools: [cargo]\nprojects: [rust]\n---\n# Review\nRun cargo test.",
            true,
        );
        install_skill(
            home.path(),
            "generic",
            "---\nkeywords: [review]\n---\n# Generic\nRead files.",
            true,
        );
        let router = SkillRouter::load(home.path()).expect("load router");
        let result = router
            .route(
                "review this rust module",
                &SkillRouteContext {
                    attachments: vec![PathBuf::from("lib.rs")],
                    expected_tools: vec!["cargo".into()],
                    project_types: vec!["rust".into()],
                    network_allowed: true,
                    destructive_allowed: false,
                },
            )
            .expect("route");
        assert_eq!(result.decisions[0].name, "Rust review");
        assert_eq!(result.decisions[0].status, SkillRouteStatus::Used);
        assert!(result.instructions.contains("Run cargo test"));
    }

    #[test]
    fn disabled_and_conflicting_skills_are_not_injected() {
        let home = tempfile::tempdir().expect("temporary home");
        install_skill(
            home.path(),
            "network",
            "---\nkeywords: [research]\nrisks: [network]\n---\n# Network\nFetch data.",
            true,
        );
        install_skill(
            home.path(),
            "disabled",
            "---\nkeywords: [research]\n---\n# Disabled\nHidden.",
            false,
        );
        let router = SkillRouter::load(home.path()).expect("load router");
        let result = router
            .route(
                "research",
                &SkillRouteContext {
                    network_allowed: false,
                    ..SkillRouteContext::default()
                },
            )
            .expect("route");
        assert!(result.instructions.is_empty());
        assert!(
            result
                .decisions
                .iter()
                .any(|item| item.status == SkillRouteStatus::Conflict)
        );
        assert!(
            result
                .decisions
                .iter()
                .any(|item| item.status == SkillRouteStatus::Skipped)
        );
    }

    #[test]
    fn rejects_reference_path_traversal_and_cycles() {
        let home = tempfile::tempdir().expect("temporary home");
        install_skill(
            home.path(),
            "escape",
            "---\nkeywords: [escape]\n---\n@include ../../outside.md",
            true,
        );
        fs::write(home.path().join("outside.md"), "outside").expect("write outside file");
        let router = SkillRouter::load(home.path()).expect("load router");
        let result = router
            .route("escape", &SkillRouteContext::default())
            .expect("an invalid skill must not abort the task");
        assert!(result.instructions.is_empty());
        assert!(result.decisions.iter().any(|d| {
            d.status == SkillRouteStatus::Skipped
                && d.conflict
                    .as_deref()
                    .is_some_and(|c| c.contains("escapes allowed path"))
        }));
    }
}
