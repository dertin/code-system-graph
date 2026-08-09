//! Captures optional Git provenance for auditable local plugin installation metadata.

use std::env;
use std::path::Path;
use std::process::Command;

const COMMIT_ENV: &str = "CODE_SYSTEM_GRAPH_BUILD_GIT_COMMIT";
const DIRTY_ENV: &str = "CODE_SYSTEM_GRAPH_BUILD_GIT_DIRTY";

fn main() {
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
