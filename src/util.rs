//! Small helpers: paths, versions, subprocesses.

use anyhow::{Context, Result, bail};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME is not set"))
}

/// Expand `~`, `~/…` and the `{claude_tmp}` placeholder (Claude Code's per-user temp folder).
pub fn expand(p: &str) -> PathBuf {
    let uid = unsafe { libc::getuid() };
    let s = p.replace("{claude_tmp}", &format!("/private/tmp/claude-{uid}"));
    if s == "~" {
        return home();
    }
    match s.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(s),
    }
}

/// Run a command to completion. On failure, the error carries its first stderr line.
pub fn run(cmd: &mut Command) -> Result<String> {
    let prog = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd.output().with_context(|| format!("could not run {prog}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        let why = if line.is_empty() { out.status.to_string() } else { line.to_string() };
        bail!("{prog}: {why}");
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Find a program on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|paths| std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()))
}

fn numbers(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

/// Compare dotted versions numerically ("0.9.1" < "0.10.0"; "1.0" == "1.0.0").
pub fn cmp_versions(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (numbers(a), numbers(b));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x.cmp(&y)
}

pub fn is_current(have: Option<&str>, latest: &str) -> bool {
    have.is_some_and(|h| cmp_versions(h, latest) != Ordering::Less)
}

/// Replace `dst` with `staged` (both directories on the same volume) with a short swap.
pub fn swap_dir(staged: &Path, dst: &Path) -> Result<()> {
    if dst.exists() {
        let old = sibling(dst, "craftctl-old");
        remove_dir_if_exists(&old)?;
        std::fs::rename(dst, &old).with_context(|| format!("moving aside {}", dst.display()))?;
        std::fs::rename(staged, dst).with_context(|| format!("installing {}", dst.display()))?;
        // Processes still running from the old copy keep their open files.
        remove_dir_if_exists(&old)?;
    } else {
        std::fs::rename(staged, dst).with_context(|| format!("installing {}", dst.display()))?;
    }
    Ok(())
}

/// A hidden sibling path: `/a/B.app` -> `/a/.B.app.<tag>`.
pub fn sibling(p: &Path, tag: &str) -> PathBuf {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    p.with_file_name(format!(".{name}.{tag}"))
}

pub fn remove_dir_if_exists(p: &Path) -> Result<()> {
    if p.exists() {
        std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
    }
    Ok(())
}

pub fn first_line(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(cmp_versions("0.9.1", "0.10.0"), Ordering::Less);
        assert_eq!(cmp_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(cmp_versions("v0.42.0", "0.41.9"), Ordering::Greater);
        assert!(is_current(Some("0.20.1"), "0.20.1"));
        assert!(!is_current(Some("0.20.1"), "0.21.0"));
        assert!(!is_current(None, "0.1.0"));
    }

    #[test]
    fn expands_home_and_placeholders() {
        assert_eq!(expand("~/x"), home().join("x"));
        assert!(expand("{claude_tmp}").to_string_lossy().starts_with("/private/tmp/claude-"));
        assert_eq!(expand("/abs"), PathBuf::from("/abs"));
    }

    #[test]
    fn sibling_is_hidden() {
        assert_eq!(sibling(Path::new("/A/B.app"), "new"), PathBuf::from("/A/.B.app.new"));
    }
}
