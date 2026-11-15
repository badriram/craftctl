//! Installing and updating apps. Each platform module provides the same four functions:
//! `gui_version`, `cli_version`, `cli_bin` and `update`.

use crate::catalog::App;
use crate::config::Config;
use crate::net::{Http, Release};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
compile_error!("craftctl supports macOS and Linux");

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub http: &'a Http,
    pub dry_run: bool,
}

/// Version of the installed GUI app (on Linux: of the app with a menu entry), if any.
pub fn gui_version(cfg: &Config, app: &App) -> Option<String> {
    platform::gui_version(cfg, app)
}

pub fn cli_version(cfg: &Config, app: &App) -> Option<String> {
    platform::cli_version(cfg, app)
}

/// The app's `<binary>-cli`, which serves MCP.
pub fn cli_bin(cfg: &Config, app: &App) -> PathBuf {
    platform::cli_bin(cfg, app)
}

/// Install or update everything for one app. One result per step, for the report line.
pub fn update(ctx: &Ctx, app: &App, rel: &Release) -> Vec<Result<String>> {
    platform::update(ctx, app, rel)
}

pub(crate) fn read_version(file: &Path) -> Option<String> {
    let v = std::fs::read_to_string(file).ok()?;
    Some(v.trim().to_string()).filter(|v| !v.is_empty())
}

/// True if a process is running from this path.
pub(crate) fn is_running(exe: &Path) -> bool {
    std::process::Command::new("pgrep")
        .arg("-f")
        .arg(exe)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Symlink `<bin_dir>/<name>` to `target`. Never replaces a real file.
pub(crate) fn link(cfg: &Config, target: &Path) -> Result<()> {
    let dir = cfg.bin_dir();
    std::fs::create_dir_all(&dir)?;
    let ln = dir.join(target.file_name().context("link target has no name")?);
    match std::fs::symlink_metadata(&ln) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(&ln)? == target {
                return Ok(());
            }
            std::fs::remove_file(&ln)?;
        }
        Ok(_) => {
            eprintln!("warning: {} exists and is not a symlink; left alone", ln.display());
            return Ok(());
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, &ln)?;
    Ok(())
}
