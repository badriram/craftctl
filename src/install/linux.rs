//! Linux: the release tarball (`<id>-<ver>-linux-<arch>.tar.gz`, a `bin/` + `share/` prefix
//! holding both the app and its CLI) unpacked per user into `<install_root>/apps/<app>/`, no
//! root needed. With `install_apps`, the app also gets a menu entry, icons and file types in
//! `~/.local/share`, pointing at that copy.
//!
//! Each tarball must match the release's SHA256SUMS.txt, and for official apps also carry a
//! valid OpenPGP signature from ArtCraft's pinned Linux release key.

use super::{Ctx, is_running, link, read_version};
use crate::catalog::App;
use crate::config::Config;
use crate::net::Release;
use crate::util::{home, is_current, swap_dir};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Integrated menu entries are listed here, inside the app folder.
const DESKTOP_RECORD: &str = ".desktop-files";

fn app_dir(cfg: &Config, app: &App) -> PathBuf {
    cfg.install_root().join("apps").join(&app.id)
}

fn gui_bin(cfg: &Config, app: &App) -> PathBuf {
    app_dir(cfg, app).join("bin").join(&app.binary)
}

pub fn cli_bin(cfg: &Config, app: &App) -> PathBuf {
    app_dir(cfg, app).join("bin").join(format!("{}-cli", app.binary))
}

pub fn cli_version(cfg: &Config, app: &App) -> Option<String> {
    read_version(&app_dir(cfg, app).join(".version"))
}

pub fn gui_version(cfg: &Config, app: &App) -> Option<String> {
    app_dir(cfg, app).join(DESKTOP_RECORD).is_file().then(|| cli_version(cfg, app)).flatten()
}

fn data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => home().join(".local/share"),
    }
}

fn arch() -> &'static str {
    std::env::consts::ARCH // "x86_64" or "aarch64", as the release names use
}

pub fn update(ctx: &Ctx, app: &App, rel: &Release) -> Vec<Result<String>> {
    let tree = update_tree(ctx, app, rel).map_err(|e| e.context("install"));
    let changed = matches!(&tree, Ok(s) if s.contains("installed"));
    let mut steps = vec![tree];
    if ctx.cfg.install_apps && cli_bin(ctx.cfg, app).is_file() {
        steps.push(integrate(ctx, app, changed).map_err(|e| e.context("menu")));
    }
    steps
}

fn update_tree(ctx: &Ctx, app: &App, rel: &Release) -> Result<String> {
    let have = cli_version(ctx.cfg, app);
    if is_current(have.as_deref(), &rel.version) && cli_bin(ctx.cfg, app).is_file() {
        if !ctx.dry_run {
            link_all(ctx.cfg, app)?;
        }
        return Ok(format!("{} ok", have.unwrap_or_default()));
    }
    let from = have.clone().unwrap_or_else(|| "-".into());
    let asset = format!("{}-{}-linux-{}.tar.gz", app.id, rel.version, arch());
    if rel.asset(&asset).is_none() {
        return Ok(format!("no Linux {} build in release", arch()));
    }
    if have.is_some() && is_running(&gui_bin(ctx.cfg, app)) {
        return Ok(format!("{from} -> {} skipped (app is running; quit it and re-run)", rel.version));
    }
    if ctx.dry_run {
        return Ok(format!("{from} -> {} (dry run)", rel.version));
    }
    let root = ctx.cfg.install_root().join("apps");
    std::fs::create_dir_all(&root)?;
    // Stage inside the apps folder so the final rename never crosses filesystems.
    let tmp = tempfile::Builder::new().prefix(".craftctl-").tempdir_in(&root)?;
    let tarball = ctx.http.fetch_verified(rel, &asset, tmp.path())?;
    let checked = match &app.linux_key {
        Some(fpr) => {
            let sig_name = format!("{asset}.asc");
            let sig =
                rel.asset(&sig_name).with_context(|| format!("{sig_name} is missing from the release"))?;
            let sig_path = tmp.path().join(&sig_name);
            std::fs::write(&sig_path, ctx.http.get_bytes(&sig.url, 64 << 10)?)?;
            crate::pgp::verify_detached(&tarball, &sig_path, fpr)?;
            "signature ok"
        }
        None => "checksum only: publisher signs no Linux builds",
    };
    let out = tmp.path().join("x");
    unpack(&tarball, &out).with_context(|| format!("unpacking {asset}"))?;
    let top = prefix_root(&out).with_context(|| format!("{asset} has no bin/ folder"))?;
    let cli = top.join("bin").join(format!("{}-cli", app.binary));
    if !cli.is_file() {
        bail!("{} not found in {asset}", cli.file_name().unwrap_or_default().to_string_lossy());
    }
    std::fs::write(top.join(".version"), format!("{}\n", rel.version))?;
    if let Ok(record) = std::fs::read(app_dir(ctx.cfg, app).join(DESKTOP_RECORD)) {
        std::fs::write(top.join(DESKTOP_RECORD), record)?; // keep the menu record across updates
    }
    swap_dir(&top, &app_dir(ctx.cfg, app))?;
    link_all(ctx.cfg, app)?;
    Ok(format!("{from} -> {} installed ({checked})", rel.version))
}

fn unpack(tarball: &Path, out: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(std::fs::File::open(tarball)?);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(true);
    archive.set_overwrite(false);
    std::fs::create_dir_all(out)?;
    // `unpack` refuses entries that would land outside `out`.
    archive.unpack(out)?;
    Ok(())
}

/// The unpacked prefix: `out` itself, or its single top-level folder, whichever holds `bin/`.
fn prefix_root(out: &Path) -> Option<PathBuf> {
    if out.join("bin").is_dir() {
        return Some(out.to_path_buf());
    }
    let mut dirs = std::fs::read_dir(out).ok()?.flatten().filter(|e| e.path().is_dir());
    let only = dirs.next()?.path();
    (dirs.next().is_none() && only.join("bin").is_dir()).then_some(only)
}

fn link_all(cfg: &Config, app: &App) -> Result<()> {
    link(cfg, &cli_bin(cfg, app))?;
    if cfg.install_apps && gui_bin(cfg, app).is_file() {
        link(cfg, &gui_bin(cfg, app))?;
    }
    Ok(())
}

/// Desktop Entry `Exec`/`TryExec`: replace the program with the installed binary, keep the
/// arguments (`%F` and so on).
pub fn rewrite_desktop(text: &str, exe: &Path) -> String {
    let exe = exe.to_string_lossy();
    let quoted = if exe.contains(' ') { format!("\"{exe}\"") } else { exe.into_owned() };
    let mut out = String::with_capacity(text.len() + 64);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Exec=") {
            let args = rest.split_once(' ').map(|(_, a)| format!(" {a}")).unwrap_or_default();
            out.push_str(&format!("Exec={quoted}{args}"));
        } else if line.starts_with("TryExec=") {
            out.push_str(&format!("TryExec={quoted}"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let (s, d) = (entry.path(), dst.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_tree(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

/// Menu entry, icons and MIME types in `~/.local/share`, pointing at the installed copy.
fn integrate(ctx: &Ctx, app: &App, changed: bool) -> Result<String> {
    let dir = app_dir(ctx.cfg, app);
    let record = dir.join(DESKTOP_RECORD);
    if record.is_file() && !changed {
        return Ok("menu ok".into());
    }
    if ctx.dry_run {
        return Ok("menu entry (dry run)".into());
    }
    let data = data_home();
    let apps_dir = data.join("applications");
    std::fs::create_dir_all(&apps_dir)?;
    let mut written = vec![];
    if let Ok(entries) = std::fs::read_dir(dir.join("share/applications")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().ends_with(".desktop") {
                continue;
            }
            let text = std::fs::read_to_string(entry.path())?;
            std::fs::write(apps_dir.join(&name), rewrite_desktop(&text, &gui_bin(ctx.cfg, app)))?;
            written.push(name.to_string_lossy().into_owned());
        }
    }
    copy_tree(&dir.join("share/icons"), &data.join("icons"))?;
    copy_tree(&dir.join("share/mime/packages"), &data.join("mime/packages"))?;
    copy_tree(&dir.join("share/metainfo"), &data.join("metainfo"))?;
    // Refresh the desktop's caches; harmless when a tool is missing.
    let _ = Command::new("update-desktop-database").arg(&apps_dir).output();
    let _ = Command::new("update-mime-database").arg(data.join("mime")).output();
    let _ = Command::new("gtk-update-icon-cache").args(["-f", "-t"]).arg(data.join("icons/hicolor")).output();
    std::fs::write(&record, written.join("\n") + "\n")?;
    Ok(if written.is_empty() { "no menu entry in package".into() } else { "menu entry installed".into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_exec_points_at_installed_binary() {
        let text = "[Desktop Entry]\nName=CADCraft\nExec=cadcraft %F\nTryExec=cadcraft\nIcon=ai.storyteller.cadcraft\n";
        let out =
            rewrite_desktop(text, Path::new("/home/u/.local/share/artcraft/apps/cadcraft/bin/cadcraft"));
        assert!(out.contains("Exec=/home/u/.local/share/artcraft/apps/cadcraft/bin/cadcraft %F\n"));
        assert!(out.contains("TryExec=/home/u/.local/share/artcraft/apps/cadcraft/bin/cadcraft\n"));
        assert!(out.contains("Icon=ai.storyteller.cadcraft\n"));
        let spaced = rewrite_desktop("Exec=x\n", Path::new("/a b/x"));
        assert_eq!(spaced, "Exec=\"/a b/x\"\n");
    }

    #[test]
    fn finds_prefix_root() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("app-1.0-linux-x86_64/bin")).unwrap();
        assert_eq!(prefix_root(t.path()), Some(t.path().join("app-1.0-linux-x86_64")));
    }
}
