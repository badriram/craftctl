//! Installing and updating GUI apps (DMG) and CLIs (zip) on macOS.

use crate::catalog::App;
use crate::config::Config;
use crate::net::{Http, Release};
use crate::util::{is_current, remove_dir_if_exists, run, sibling, swap_dir};
use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub http: &'a Http,
    pub dry_run: bool,
}

pub fn app_path(app: &App) -> PathBuf {
    Path::new("/Applications").join(format!("{}.app", app.bundle_name))
}

pub fn installed_app_version(app: &App) -> Option<String> {
    let v = plist::Value::from_file(app_path(app).join("Contents/Info.plist")).ok()?;
    Some(v.as_dictionary()?.get("CFBundleShortVersionString")?.as_string()?.to_string())
}

pub fn cli_dir(cfg: &Config, app: &App) -> PathBuf {
    cfg.cli_root().join(&app.id)
}

pub fn cli_bin(cfg: &Config, app: &App) -> PathBuf {
    cli_dir(cfg, app).join(format!("{}-cli", app.binary))
}

pub fn installed_cli_version(cfg: &Config, app: &App) -> Option<String> {
    let v = std::fs::read_to_string(cli_dir(cfg, app).join(".version")).ok()?;
    Some(v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Code signature must be valid and from `team`; bundles must also pass Gatekeeper (notarized).
pub fn verify_signature(path: &Path, team: &str, bundle: bool) -> Result<()> {
    let mut check = Command::new("codesign");
    check.args(["--verify", "--deep"]);
    if !bundle {
        // --strict rejects the Finder metadata a mounted DMG exposes, so bundles skip it;
        // spctl below is the authoritative check for them.
        check.arg("--strict");
    }
    run(check.arg(path))?;
    let out = Command::new("codesign").arg("-dv").arg(path).output()?;
    let info = String::from_utf8_lossy(&out.stderr);
    let got = info.lines().find_map(|l| l.strip_prefix("TeamIdentifier=")).map(str::trim);
    if got != Some(team) {
        bail!("{} is signed by team {}, expected {team}", path.display(), got.unwrap_or("none"));
    }
    if bundle {
        run(Command::new("spctl").args(["--assess", "--type", "execute"]).arg(path))?;
    }
    Ok(())
}

fn is_running(app: &App) -> bool {
    let exe = app_path(app).join("Contents/MacOS");
    Command::new("pgrep").arg("-f").arg(exe).output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn update_app(ctx: &Ctx, app: &App, rel: &Release) -> Result<String> {
    let have = installed_app_version(app);
    if is_current(have.as_deref(), &rel.version) {
        return Ok(format!("app {} ok", have.unwrap_or_default()));
    }
    let from = have.clone().unwrap_or_else(|| "-".into());
    let asset = format!("{}-{}-macos-universal.dmg", app.id, rel.version);
    if rel.asset(&asset).is_none() {
        return Ok("app: no macOS build in release".into());
    }
    if have.is_some() && is_running(app) {
        return Ok(format!("app {from} -> {} skipped (running; quit it and re-run)", rel.version));
    }
    if ctx.dry_run {
        return Ok(format!("app {from} -> {} (dry run)", rel.version));
    }
    let tmp = tempfile::Builder::new().prefix("craftctl-").tempdir()?;
    let dmg = ctx.http.fetch_verified(rel, &asset, tmp.path())?;
    let mnt = tmp.path().join("mnt");
    std::fs::create_dir(&mnt)?;
    run(Command::new("hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
        .arg(&mnt)
        .arg(&dmg))?;
    let staged = (|| -> Result<PathBuf> {
        let src = mnt.join(format!("{}.app", app.bundle_name));
        if !src.is_dir() {
            bail!("{}.app not found in {asset}", app.bundle_name);
        }
        verify_signature(&src, &app.team, true)?;
        let staged = sibling(&app_path(app), "craftctl-new");
        remove_dir_if_exists(&staged)?;
        run(Command::new("ditto").arg(&src).arg(&staged))?;
        Ok(staged)
    })();
    let _ = Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mnt).status();
    swap_dir(&staged?, &app_path(app))?;
    Ok(format!("app {from} -> {} installed", rel.version))
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        let ft = entry.file_type().ok()?;
        if ft.is_file() && p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
        if ft.is_dir()
            && let Some(found) = find_file(&p, name)
        {
            return Some(found);
        }
    }
    None
}

pub fn update_cli(ctx: &Ctx, app: &App, rel: &Release) -> Result<String> {
    let have = installed_cli_version(ctx.cfg, app);
    if is_current(have.as_deref(), &rel.version) && cli_bin(ctx.cfg, app).is_file() {
        if !ctx.dry_run {
            link(ctx.cfg, app)?;
        }
        return Ok(format!("cli {} ok", have.unwrap_or_default()));
    }
    let from = have.unwrap_or_else(|| "-".into());
    let asset = format!("{}-cli-{}-macos-universal.zip", app.id, rel.version);
    if rel.asset(&asset).is_none() {
        return Ok("cli: no macOS CLI in release".into());
    }
    if ctx.dry_run {
        return Ok(format!("cli {from} -> {} (dry run)", rel.version));
    }
    let root = ctx.cfg.cli_root();
    std::fs::create_dir_all(&root)?;
    // Stage inside the CLI root so the final rename never crosses volumes.
    let tmp = tempfile::Builder::new().prefix(".craftctl-").tempdir_in(&root)?;
    let zip = ctx.http.fetch_verified(rel, &asset, tmp.path())?;
    let out = tmp.path().join("x");
    let file = std::fs::File::open(&zip)?;
    zip::ZipArchive::new(file)?.extract(&out).with_context(|| format!("extracting {asset}"))?;
    let name = format!("{}-cli", app.binary);
    let bin = find_file(&out, &name).with_context(|| format!("{name} not found in {asset}"))?;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
    verify_signature(&bin, &app.team, false)?;
    let folder = bin.parent().context("CLI has no folder")?;
    std::fs::write(folder.join(".version"), format!("{}\n", rel.version))?;
    swap_dir(folder, &cli_dir(ctx.cfg, app))?;
    link(ctx.cfg, app)?;
    Ok(format!("cli {from} -> {} installed", rel.version))
}

/// Symlink `<bin_dir>/<binary>-cli` to the managed CLI. Never replaces a real file.
fn link(cfg: &Config, app: &App) -> Result<()> {
    let target = cli_bin(cfg, app);
    let dir = cfg.bin_dir();
    std::fs::create_dir_all(&dir)?;
    let ln = dir.join(target.file_name().context("CLI has no name")?);
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
    std::os::unix::fs::symlink(&target, &ln)?;
    Ok(())
}
