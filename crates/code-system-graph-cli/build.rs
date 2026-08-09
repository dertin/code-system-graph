//! Captures optional Git provenance for auditable local plugin installation metadata.

use std::env;
use std::path::Path;
use std::process::Command;

const COMMIT_ENV: &str = "CODE_SYSTEM_GRAPH_BUILD_GIT_COMMIT";
const DIRTY_ENV: &str = "CODE_SYSTEM_GRAPH_BUILD_GIT_DIRTY";

fn main() {
    emit_target_linker_configuration();

    println!("cargo:rerun-if-env-changed={COMMIT_ENV}");
    println!("cargo:rerun-if-env-changed={DIRTY_ENV}");

    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let manifest_dir = Path::new(&manifest_dir);
    let commit = env::var(COMMIT_ENV)
        .ok()
        .or_else(|| git_output(manifest_dir, &["rev-parse", "--verify", "HEAD"]));
    if let Some(commit) = commit.filter(|value| valid_commit(value)) {
        println!("cargo:rustc-env={COMMIT_ENV}={commit}");
    }

    let dirty = env::var(DIRTY_ENV).ok().or_else(|| {
        git_output(
            manifest_dir,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )
        .map(|status| (!status.is_empty()).to_string())
    });
    if let Some(dirty) = dirty.filter(|value| matches!(value.as_str(), "true" | "false")) {
        println!("cargo:rustc-env={DIRTY_ENV}={dirty}");
    }

    if let Some(git_dir) = git_output(manifest_dir, &["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/index");
    }

    emit_worktree_rerun_triggers(manifest_dir);
}

fn emit_target_linker_configuration() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // MSVC executables default to a 1 MiB main stack, which is insufficient for the
        // synchronous extraction worker entered from the async CLI dispatcher.
        println!("cargo:rustc-link-arg-bin=csgraph=/STACK:8388608");
    }
}

fn emit_worktree_rerun_triggers(manifest_dir: &Path) {
    let Some(worktree_root) = git_output(manifest_dir, &["rev-parse", "--show-toplevel"]) else {
        return;
    };
    let worktree_root = Path::new(&worktree_root);
    let Some(paths) = git_output(
        worktree_root,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    ) else {
        return;
    };

    // Explicit Git metadata triggers do not observe unstaged edits. Watching every materialized
    // tracked or untracked source path keeps the embedded dirty flag aligned with the binary that
    // Cargo is compiling. Index changes cover newly staged paths, while edits that make a new
    // source file reachable necessarily also touch an already watched tracked file.
    for relative in paths.lines().filter(|path| !path.is_empty()) {
        println!(
            "cargo:rerun-if-changed={}",
            worktree_root.join(relative).display()
        );
    }
}

fn git_output(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
