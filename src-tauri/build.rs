//! Tauri's build script, plus a build stamp.
//!
//! **Why a stamp.** Verifying which binary is installed should not be guesswork. The user
//! dragged a DMG, believed it had replaced the application, and was running a build from
//! seven minutes earlier — which is invisible until something behaves oddly and you cannot
//! tell whether you are testing the fix or the bug.
//!
//! The commit and the build time are compiled into the binary and surfaced in the
//! Capability Report, so "which build is this?" has an answer that does not depend on file
//! timestamps or trust.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // `git status --porcelain` is empty when the tree is clean, which is worth saying: a
    // binary built from a dirty tree matches no commit, and saying so prevents a confusing
    // hunt later.
    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    println!("cargo:rustc-env=CHAFF_GIT_SHA={sha}{}", if dirty { "+dirty" } else { "" });
    println!("cargo:rustc-env=CHAFF_BUILD_EPOCH={now}");

    // **Watch the ref the branch points at, not HEAD.**
    //
    // `.git/HEAD` contains `ref: refs/heads/main` and does not change when a commit is
    // made — the commit SHA is written to `.git/refs/heads/main`. Watching HEAD, which is
    // what the first version did, meant this script never re-ran on a commit and every
    // binary built after the first embedded a stale SHA.
    //
    // Found because the install script compared the stamp against `git rev-parse` and
    // reported a mismatch on a binary that had just been built. The verification caught
    // the bug in the thing it was verifying, which is the only reason it is worth having.
    let head = std::fs::read_to_string("../.git/HEAD").unwrap_or_default();
    if let Some(reference) = head.strip_prefix("ref: ") {
        println!("cargo:rerun-if-changed=../.git/{}", reference.trim());
    } else {
        // Detached HEAD: the file holds the SHA directly, so it does change.
        println!("cargo:rerun-if-changed=../.git/HEAD");
    }

    tauri_build::build();
}
