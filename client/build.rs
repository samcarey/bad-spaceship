use std::process::Command;

fn main() -> shadow_rs::SdResult<()> {
    let output = Command::new("git")
        .args(&["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let git_hash = String::from_utf8(output.stdout).unwrap();
    let short_git_hash = git_hash.get(0..7).unwrap();
    println!("cargo:rustc-env=SHORT_GIT_HASH={}", short_git_hash);

    shadow_rs::new()
}
