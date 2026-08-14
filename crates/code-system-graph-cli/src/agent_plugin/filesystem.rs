use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use atomic_write_file::AtomicWriteFile;

use super::{AgentPluginError, AgentPluginMcpBinding, conflict, recognized_binding_generator};

pub(super) fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<(), AgentPluginError> {
    let mut file = AtomicWriteFile::open(path).map_err(|source| AgentPluginError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(contents)
        .map_err(|source| AgentPluginError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.commit().map_err(|source| AgentPluginError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub(super) fn verify_existing(
    output: &Path,
    expected: &BTreeMap<String, Vec<u8>>,
) -> Result<(), AgentPluginError> {
    let metadata = fs::symlink_metadata(output).map_err(|source| AgentPluginError::Resolve {
        path: output.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(conflict(
            output,
            "output is a symlink or is not a directory",
        ));
    }
    let actual = collect_relative_files(output, output)?;
    let expected_paths = expected_entries(expected);
    if actual != expected_paths {
        return Err(conflict(
            output,
            "files are missing, additional, or replaced by symlinks",
        ));
    }
    for (relative, contents) in expected {
        let path = output.join(relative);
        let actual = fs::read(&path).map_err(|source| AgentPluginError::Resolve {
            path: path.clone(),
            source,
        })?;
        if actual != *contents {
            return Err(conflict(output, &format!("`{relative}` differs")));
        }
    }
    Ok(())
}

fn expected_entries(files: &BTreeMap<String, Vec<u8>>) -> BTreeSet<String> {
    let mut entries = BTreeSet::new();
    for relative in files.keys() {
        entries.insert(relative.clone());
        let mut parent = Path::new(relative).parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            entries.insert(format!(
                "{}/",
                directory.to_string_lossy().replace('\\', "/")
            ));
            parent = directory.parent();
        }
    }
    entries
}

fn collect_relative_files(
    root: &Path,
    directory: &Path,
) -> Result<BTreeSet<String>, AgentPluginError> {
    let mut result = BTreeSet::new();
    let entries = fs::read_dir(directory).map_err(|source| AgentPluginError::Resolve {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| AgentPluginError::Resolve {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| AgentPluginError::Resolve {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(conflict(root, "symlinks are not allowed"));
        }
        if metadata.is_dir() {
            let relative = path.strip_prefix(root).expect("child remains below root");
            result.insert(format!(
                "{}/",
                relative.to_string_lossy().replace('\\', "/")
            ));
            result.extend(collect_relative_files(root, &path)?);
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).expect("child remains below root");
            result.insert(relative.to_string_lossy().replace('\\', "/"));
        } else {
            return Err(conflict(root, "non-regular entries are not allowed"));
        }
    }
    Ok(result)
}

pub(super) fn write_new_atomically(
    output: &Path,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), AgentPluginError> {
    let parent = output.parent().expect("absolute output has parent");
    let name = output
        .file_name()
        .expect("validated output name")
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let temporary = parent.join(format!(".{name}.tmp-{}-{nonce}", std::process::id()));
    fs::create_dir(&temporary).map_err(|source| AgentPluginError::Write {
        path: temporary.clone(),
        source,
    })?;
    let result = (|| {
        for (relative, contents) in files {
            let path = temporary.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| AgentPluginError::Write {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&path, contents).map_err(|source| AgentPluginError::Write {
                path: path.clone(),
                source,
            })?;
        }
        fs::rename(&temporary, output).map_err(|source| AgentPluginError::Write {
            path: output.to_path_buf(),
            source,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

pub(super) fn verify_binding_ownership(output: &Path) -> Result<(), AgentPluginError> {
    let binding_path = output.join("mcp-binding.json");
    let metadata = fs::symlink_metadata(&binding_path)
        .map_err(|_| conflict(output, "replacement requires an owned `mcp-binding.json`"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(conflict(output, "binding ownership file is not regular"));
    }
    let binding: AgentPluginMcpBinding =
        serde_json::from_slice(&fs::read(&binding_path).map_err(|source| {
            AgentPluginError::Resolve {
                path: binding_path,
                source,
            }
        })?)
        .map_err(|_| conflict(output, "binding ownership identity is malformed"))?;
    if binding.schema_version != 2 || !recognized_binding_generator(&binding.generator) {
        return Err(conflict(
            output,
            "binding ownership identity is unrecognized",
        ));
    }
    Ok(())
}

pub(super) fn replace_owned_atomically(
    output: &Path,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), AgentPluginError> {
    let parent = output.parent().expect("absolute output has parent");
    let name = output
        .file_name()
        .expect("validated output name")
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let staged = parent.join(format!(".{name}.replace-{nonce}"));
    let backup = parent.join(format!(".{name}.previous-{nonce}"));
    write_new_atomically(&staged, files)?;
    fs::rename(output, &backup).map_err(|source| AgentPluginError::Write {
        path: output.to_path_buf(),
        source,
    })?;
    if let Err(source) = fs::rename(&staged, output) {
        let _ = fs::rename(&backup, output);
        let _ = fs::remove_dir_all(&staged);
        return Err(AgentPluginError::Write {
            path: output.to_path_buf(),
            source,
        });
    }
    fs::remove_dir_all(&backup).map_err(|source| AgentPluginError::Write {
        path: backup,
        source,
    })?;
    Ok(())
}
