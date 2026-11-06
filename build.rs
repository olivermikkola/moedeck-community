//! Stamp the commit MoE Deck was built from, so Settings can tell whether `main`
//! has moved on. Builds outside a git checkout get an empty stamp.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let sha = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=MOEDECK_GIT_SHA={sha}");
    // Re-stamp after a commit or checkout, not only when sources change.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &branch]) {
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
