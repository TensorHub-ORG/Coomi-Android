use coomi_security::Decision;
use coomi_security::SecurityPolicy;
use coomi_services::RuntimePathMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
enum PatchOperation {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<PatchChunk>,
    },
}

#[derive(Clone, Debug, Default)]
struct PatchChunk {
    change_context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    end_of_file: bool,
}

#[derive(Clone)]
struct PreparedChange {
    source: PathBuf,
    destination: PathBuf,
    previous_source: Option<Vec<u8>>,
    previous_destination: Option<Vec<u8>>,
    new_content: Option<Vec<u8>>,
}

/// Adding or editing files follows the workspace write policy. Removing a file,
/// including moving it to another path, needs a separate approval decision.
pub fn removes_files(patch: &str) -> Result<bool, String> {
    Ok(parse_patch(patch)?.iter().any(|operation| match operation {
        PatchOperation::Delete { .. } => true,
        PatchOperation::Update {
            path,
            move_to: Some(destination),
            ..
        } => path != destination,
        _ => false,
    }))
}

#[allow(dead_code)]
pub fn apply_patch(policy: &SecurityPolicy, patch: &str) -> Result<String, String> {
    apply_patch_with_paths(policy, None, patch)
}

pub fn apply_patch_with_paths(
    policy: &SecurityPolicy,
    path_map: Option<&RuntimePathMap>,
    patch: &str,
) -> Result<String, String> {
    apply_patch_with_journal(policy, path_map, patch, None)
}

pub fn apply_patch_with_journal(
    policy: &SecurityPolicy,
    path_map: Option<&RuntimePathMap>,
    patch: &str,
    journal: Option<&coomi_services::FileJournal>,
) -> Result<String, String> {
    let operations = parse_patch(patch)?;
    if operations.is_empty() {
        return Err("patch contains no file operations".into());
    }
    let mut prepared = Vec::with_capacity(operations.len());
    for operation in operations {
        prepared.push(prepare_change(policy, path_map, operation)?);
    }
    if let Some(journal) = journal {
        let in_workspace = prepared.iter().all(|change| {
            journal.relative_path(&change.source).is_ok()
                && journal.relative_path(&change.destination).is_ok()
        });
        if in_workspace {
            let mut edits = Vec::new();
            for change in &prepared {
                if change.source != change.destination {
                    edits.push(coomi_services::FileEdit {
                        path: change.source.clone(),
                        before: change.previous_source.clone(),
                        after: None,
                        after_mode: None,
                    });
                    edits.push(coomi_services::FileEdit {
                        path: change.destination.clone(),
                        before: change.previous_destination.clone(),
                        after: change.new_content.clone(),
                        after_mode: None,
                    });
                } else {
                    edits.push(coomi_services::FileEdit {
                        path: change.source.clone(),
                        before: change.previous_source.clone(),
                        after: change.new_content.clone(),
                        after_mode: None,
                    });
                }
            }
            journal.apply(edits).map_err(|error| format!("{error:#}"))?;
        } else {
            journal
                .mark_untracked("本轮包含工作区外或不支持备份的补丁路径，该补丁不会自动回撤。")
                .map_err(|error| format!("{error:#}"))?;
            apply_prepared(&prepared)?;
        }
    } else {
        apply_prepared(&prepared)?;
    }

    let mut output = Vec::new();
    for change in prepared {
        let display = change
            .destination
            .strip_prefix(policy.workspace())
            .unwrap_or(&change.destination);
        let action = if change.new_content.is_none() {
            "Deleted"
        } else if change.previous_source.is_none() {
            "Added"
        } else {
            "Updated"
        };
        output.push(format!("{action} {}", display.display()));
    }
    Ok(output.join("\n"))
}

fn prepare_change(
    policy: &SecurityPolicy,
    path_map: Option<&RuntimePathMap>,
    operation: PatchOperation,
) -> Result<PreparedChange, String> {
    match operation {
        PatchOperation::Add { path, content } => {
            let path = checked_write_path(policy, path_map, &path)?;
            Ok(PreparedChange {
                source: path.clone(),
                destination: path.clone(),
                previous_source: fs::read(&path).ok(),
                previous_destination: None,
                new_content: Some(content.into_bytes()),
            })
        }
        PatchOperation::Delete { path } => {
            let path = checked_write_path(policy, path_map, &path)?;
            let previous = fs::read(&path)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            Ok(PreparedChange {
                source: path.clone(),
                destination: path,
                previous_source: Some(previous),
                previous_destination: None,
                new_content: None,
            })
        }
        PatchOperation::Update {
            path,
            move_to,
            chunks,
        } => {
            let source = checked_write_path(policy, path_map, &path)?;
            let destination = match move_to {
                Some(path) => checked_write_path(policy, path_map, &path)?,
                None => source.clone(),
            };
            let previous = fs::read(&source)
                .map_err(|error| format!("failed to read {}: {error}", source.display()))?;
            let text = String::from_utf8(previous.clone())
                .map_err(|_| format!("{} is not UTF-8 text", source.display()))?;
            let updated = apply_chunks(&text, &chunks)?;
            let previous_destination = (destination != source)
                .then(|| fs::read(&destination).ok())
                .flatten();
            Ok(PreparedChange {
                source,
                destination,
                previous_source: Some(previous),
                previous_destination,
                new_content: Some(updated.into_bytes()),
            })
        }
    }
}

fn checked_write_path(
    policy: &SecurityPolicy,
    path_map: Option<&RuntimePathMap>,
    value: &str,
) -> Result<PathBuf, String> {
    let host = path_map
        .map(|map| map.resolve(value, None).map(|resolved| resolved.host_path))
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| PathBuf::from(value));
    let path = policy
        .resolve_path(&host)
        .map_err(|error| error.to_string())?;
    match policy.assess_write(&path) {
        Decision::Allow => Ok(path),
        Decision::Ask(reason) | Decision::Deny(reason) => Err(reason),
    }
}

fn apply_prepared(changes: &[PreparedChange]) -> Result<(), String> {
    let mut applied = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let result = if let Some(content) = &change.new_content {
            change
                .destination
                .parent()
                .map(fs::create_dir_all)
                .transpose()
                .and_then(|_| fs::write(&change.destination, content))
                .and_then(|_| {
                    if change.source != change.destination && change.source.exists() {
                        fs::remove_file(&change.source)
                    } else {
                        Ok(())
                    }
                })
        } else {
            fs::remove_file(&change.source)
        };
        match result {
            Ok(()) => applied.push(index),
            Err(error) => {
                for applied_index in applied.into_iter().rev() {
                    restore(&changes[applied_index]);
                }
                return Err(format!(
                    "failed to apply patch to {}: {error}",
                    change.destination.display()
                ));
            }
        }
    }
    Ok(())
}

fn restore(change: &PreparedChange) {
    if change.source != change.destination {
        match &change.previous_destination {
            Some(content) => {
                let _ = fs::write(&change.destination, content);
            }
            None => {
                let _ = fs::remove_file(&change.destination);
            }
        }
    }
    match &change.previous_source {
        Some(content) => {
            if let Some(parent) = change.source.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&change.source, content);
        }
        None => {
            let _ = fs::remove_file(&change.source);
        }
    }
}

fn apply_chunks(content: &str, chunks: &[PatchChunk]) -> Result<String, String> {
    let had_newline = content.ends_with('\n');
    let mut lines = content.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut cursor = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            let relative = find_sequence(&lines[cursor..], std::slice::from_ref(context))
                .ok_or_else(|| format!("patch context not found: {context}"))?;
            cursor = cursor.saturating_add(relative + 1);
        }
        let position = if chunk.old_lines.is_empty() {
            if chunk.end_of_file {
                lines.len()
            } else {
                cursor
            }
        } else {
            let search = if chunk.end_of_file {
                lines.len().saturating_sub(chunk.old_lines.len())
            } else {
                cursor
            };
            let relative = find_sequence(&lines[search..], &chunk.old_lines).ok_or_else(|| {
                format!(
                    "patch context did not match: {}",
                    chunk.old_lines.join("\\n")
                )
            })?;
            search.saturating_add(relative)
        };
        let end = position.saturating_add(chunk.old_lines.len());
        lines.splice(position..end, chunk.new_lines.clone());
        cursor = position.saturating_add(chunk.new_lines.len());
    }
    let mut output = lines.join("\n");
    if had_newline || !chunks.is_empty() {
        output.push('\n');
    }
    Ok(output)
}

fn find_sequence(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    for start in 0..=haystack.len().saturating_sub(needle.len()) {
        let candidate = &haystack[start..start + needle.len()];
        if candidate == needle
            || candidate
                .iter()
                .zip(needle)
                .all(|(left, right)| left.trim_end() == right.trim_end())
        {
            return Some(start);
        }
    }
    None
}

fn parse_patch(patch: &str) -> Result<Vec<PatchOperation>, String> {
    let lines = patch.trim().lines().collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some("*** Begin Patch") {
        return Err("first line must be `*** Begin Patch`".into());
    }
    if lines.last().map(|line| line.trim()) != Some("*** End Patch") {
        return Err("last line must be `*** End Patch`".into());
    }
    let mut operations = Vec::new();
    let mut index = 1;
    while index + 1 < lines.len() {
        let line = lines[index];
        if let Some(path) = line.strip_prefix("*** Add File: ") {
            index += 1;
            let mut content = Vec::new();
            while index + 1 < lines.len() && !lines[index].starts_with("*** ") {
                let added = lines[index]
                    .strip_prefix('+')
                    .ok_or_else(|| format!("invalid add line {}", index + 1))?;
                content.push(added);
                index += 1;
            }
            operations.push(PatchOperation::Add {
                path: path.trim().to_owned(),
                content: format!("{}\n", content.join("\n")),
            });
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            operations.push(PatchOperation::Delete {
                path: path.trim().to_owned(),
            });
            index += 1;
            continue;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            index += 1;
            let mut move_to = None;
            if index + 1 < lines.len()
                && let Some(path) = lines[index].strip_prefix("*** Move to: ")
            {
                move_to = Some(path.trim().to_owned());
                index += 1;
            }
            let mut chunks = Vec::new();
            let mut current = PatchChunk::default();
            let mut saw_content = false;
            while index + 1 < lines.len() && !is_operation_header(lines[index]) {
                let item = lines[index];
                if let Some(context) = item.strip_prefix("@@") {
                    if saw_content {
                        chunks.push(current);
                        current = PatchChunk::default();
                        saw_content = false;
                    }
                    let context = context.trim().trim_end_matches("@@").trim();
                    current.change_context = (!context.is_empty()).then(|| context.to_owned());
                } else if item == "*** End of File" {
                    current.end_of_file = true;
                } else if let Some(value) = item.strip_prefix('+') {
                    current.new_lines.push(value.to_owned());
                    saw_content = true;
                } else if let Some(value) = item.strip_prefix('-') {
                    current.old_lines.push(value.to_owned());
                    saw_content = true;
                } else if let Some(value) = item.strip_prefix(' ') {
                    current.old_lines.push(value.to_owned());
                    current.new_lines.push(value.to_owned());
                    saw_content = true;
                } else if !item.trim().is_empty() {
                    return Err(format!("invalid patch line {}: {item}", index + 1));
                }
                index += 1;
            }
            if saw_content || current.end_of_file {
                chunks.push(current);
            }
            if chunks.is_empty() {
                return Err(format!("update for `{}` contains no changes", path.trim()));
            }
            operations.push(PatchOperation::Update {
                path: path.trim().to_owned(),
                move_to,
                chunks,
            });
            continue;
        }
        if line.trim().is_empty() {
            index += 1;
            continue;
        }
        return Err(format!(
            "invalid patch operation on line {}: {line}",
            index + 1
        ));
    }
    Ok(operations)
}

fn is_operation_header(line: &str) -> bool {
    line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
        || line.trim() == "*** End Patch"
}

#[cfg(test)]
mod tests {
    use super::*;
    use coomi_security::AccessMode;

    #[test]
    fn applies_add_update_delete_as_one_validated_patch() {
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(workspace.path().join("edit.txt"), "before\n").expect("fixture");
        fs::write(workspace.path().join("delete.txt"), "gone\n").expect("fixture");
        let policy =
            SecurityPolicy::new(workspace.path(), AccessMode::WorkspaceWrite).expect("policy");
        let patch = "*** Begin Patch\n*** Add File: new.txt\n+new\n*** Update File: edit.txt\n@@\n-before\n+after\n*** Delete File: delete.txt\n*** End Patch";
        let result = apply_patch(&policy, patch).expect("apply");
        assert!(result.contains("Added new.txt"));
        assert_eq!(
            fs::read_to_string(workspace.path().join("edit.txt")).expect("read"),
            "after\n"
        );
        assert!(!workspace.path().join("delete.txt").exists());
    }

    #[test]
    fn rejects_context_mismatch_without_modifying_any_file() {
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(workspace.path().join("edit.txt"), "before\n").expect("fixture");
        let policy =
            SecurityPolicy::new(workspace.path(), AccessMode::WorkspaceWrite).expect("policy");
        let patch = "*** Begin Patch\n*** Add File: new.txt\n+new\n*** Update File: edit.txt\n@@\n-missing\n+after\n*** End Patch";
        assert!(apply_patch(&policy, patch).is_err());
        assert!(!workspace.path().join("new.txt").exists());
    }
}
