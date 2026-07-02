use std::process::Command;

/// Bake the build's git commit into the binary so the in-app "source" link can
/// permalink to exactly this revision of the crate on GitHub (same reason the
/// client injects SHORT_GIT_HASH via shadow-rs — this crate needs only the one
/// value, so a plain `git rev-parse` keeps it dependency-free).
fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        // No git (tarball build): fall back to the default branch, so the link
        // still lands on the crate directory, just not pinned.
        .unwrap_or_else(|| "master".into());
    println!("cargo:rustc-env=BUILD_COMMIT={hash}");
    // Re-run when HEAD moves. `.git/HEAD` only changes on checkout — a commit to
    // the same branch moves the ref file it points at — so track the resolved
    // ref too (and packed-refs, where refs live after `git gc`).
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/packed-refs");
    if let Ok(head) = std::fs::read_to_string("../.git/HEAD") {
        if let Some(reference) = head.strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=../.git/{}", reference.trim());
        }
    }
}
