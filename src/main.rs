//! craftctl: install, update and register MCP servers for the ArtCraft Crafting Apps.
//! Unofficial; not affiliated with ArtCraft.

mod catalog;
mod clients;
mod config;
mod install;
mod net;
mod probe;
mod util;

use anyhow::{Result, bail};
use catalog::App;
use clap::{Parser, Subcommand};
use config::Config;
use install::Ctx;
use net::{Http, Release};
use probe::Confinement;
use std::process::ExitCode;
use util::first_line;

#[derive(Parser)]
#[command(
    name = "craftctl",
    version,
    about = "Install, update and register MCP servers for the ArtCraft Crafting Apps (unofficial)"
)]
struct Cli {
    /// Show what would change without downloading or writing anything
    #[arg(long, short = 'n', global = true)]
    dry_run: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Installed versions vs. the latest releases
    Status { apps: Vec<String> },
    /// Install or update GUI apps and CLIs (default: every app not excluded)
    Update { apps: Vec<String> },
    /// Register MCP servers with the configured clients
    Mcp {
        apps: Vec<String>,
        /// Unregister these apps' servers instead
        #[arg(long, requires = "apps")]
        remove: bool,
    },
    /// update, then mcp: what topgrade runs
    Sync { apps: Vec<String> },
    /// Print the config file path and the apps the catalog offers
    Config,
}

fn select(cat: &catalog::Catalog, cfg: &Config, names: &[String]) -> Result<Vec<App>> {
    if names.is_empty() {
        return Ok(cat.apps.iter().filter(|a| !cfg.exclude.contains(&a.id)).cloned().collect());
    }
    let mut out = vec![];
    for n in names {
        match cat.apps.iter().find(|a| &a.id == n) {
            Some(a) => out.push(a.clone()),
            None => match cat.rejected.iter().find(|(id, _)| id == n) {
                Some((_, why)) => bail!("{n} is in the catalog but refused: {why}"),
                None => bail!("unknown app {n} (see `craftctl config`)"),
            },
        }
    }
    Ok(out)
}

/// Look up every app's latest release in parallel.
fn releases(http: &Http, apps: &[App]) -> Vec<Result<Release>> {
    std::thread::scope(|s| {
        let handles: Vec<_> = apps.iter().map(|a| s.spawn(|| http.latest_release(&a.repo))).collect();
        handles.into_iter().map(|h| h.join().expect("lookup thread panicked")).collect()
    })
}

fn status(ctx: &Ctx, apps: &[App]) -> u32 {
    println!("{:<15} {:>9} {:>9} {:>9}", "app", "gui", "cli", "latest");
    let mut failures = 0;
    for (app, rel) in apps.iter().zip(releases(ctx.http, apps)) {
        let gui = install::installed_app_version(app);
        let cli = install::installed_cli_version(ctx.cfg, app);
        let (latest, note) = match &rel {
            Ok(r) => {
                let gui_ok = !ctx.cfg.install_apps || util::is_current(gui.as_deref(), &r.version);
                let stale = !(gui_ok && util::is_current(cli.as_deref(), &r.version));
                (r.version.clone(), if stale { "  <- update" } else { "" }.to_string())
            }
            Err(e) => {
                failures += 1;
                ("?".into(), format!("  {}", first_line(e)))
            }
        };
        let show = |v: Option<String>| v.unwrap_or_else(|| "-".into());
        println!("{:<15} {:>9} {:>9} {:>9}{note}", app.id, show(gui), show(cli), latest);
    }
    failures
}

fn update(ctx: &Ctx, apps: &[App]) -> u32 {
    let mut failures = 0;
    for (app, rel) in apps.iter().zip(releases(ctx.http, apps)) {
        let rel = match rel {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                println!("{:<15} ERROR release lookup: {}", app.id, first_line(&e));
                continue;
            }
        };
        let mut parts = vec![];
        if ctx.cfg.install_apps {
            match install::update_app(ctx, app, &rel) {
                Ok(s) => parts.push(s),
                Err(e) => {
                    failures += 1;
                    parts.push(format!("app ERROR {}", first_line(&e)));
                }
            }
        }
        match install::update_cli(ctx, app, &rel) {
            Ok(s) => parts.push(s),
            Err(e) => {
                failures += 1;
                parts.push(format!("cli ERROR {}", first_line(&e)));
            }
        }
        println!("{:<15} {}", app.id, parts.join(" | "));
    }
    failures
}

fn mcp(ctx: &Ctx, apps: &[App], full_run: bool, remove: bool) -> u32 {
    let mut desired = vec![];
    let mut unconfined = vec![];
    if !remove {
        for app in apps.iter().filter(|a| !ctx.cfg.mcp_exclude.contains(&a.id)) {
            let bin = install::cli_bin(ctx.cfg, app);
            if !bin.is_file() {
                continue;
            }
            let (args, how) = probe::mcp_args(ctx.cfg, app, &bin);
            if how == Confinement::None {
                unconfined.push(app.id.as_str());
            }
            desired.push(clients::Server {
                name: app.id.clone(),
                command: bin.to_string_lossy().into(),
                args,
            });
        }
    }
    if !unconfined.is_empty() {
        println!(
            "note: no file-confinement option, so these servers have your full file access: {}",
            unconfined.join(", ")
        );
    }
    let names: Vec<String> = if remove { apps.iter().map(|a| a.id.clone()).collect() } else { vec![] };
    let prefix = ctx.cfg.cli_root();
    let plan = clients::Plan {
        desired: &desired,
        remove: &names,
        prune: full_run && !remove,
        managed_prefix: &prefix,
    };
    let log = clients::apply(&ctx.cfg.mcp_targets, &plan, ctx.dry_run);
    let failures = log.iter().filter(|l| l.contains("ERROR") || l.contains("FAILED")).count() as u32;
    for line in log {
        println!("  {line}");
    }
    failures
}

fn real_main() -> Result<u32> {
    let cli = Cli::parse();
    if !cfg!(target_os = "macos") {
        bail!("craftctl currently supports macOS only");
    }
    let cfg = config::load()?;
    let http = Http::new();
    let cat = catalog::load(&http)?;
    let ctx = Ctx { cfg: &cfg, http: &http, dry_run: cli.dry_run };
    if cli.dry_run {
        println!("(dry run: nothing will be downloaded or written)");
    }
    Ok(match cli.cmd {
        Cmd::Status { apps } => status(&ctx, &select(&cat, &cfg, &apps)?),
        Cmd::Update { apps } => update(&ctx, &select(&cat, &cfg, &apps)?),
        Cmd::Mcp { apps, remove } => mcp(&ctx, &select(&cat, &cfg, &apps)?, apps.is_empty(), remove),
        Cmd::Sync { apps } => {
            let selected = select(&cat, &cfg, &apps)?;
            update(&ctx, &selected) + mcp(&ctx, &selected, apps.is_empty(), false)
        }
        Cmd::Config => {
            println!("config: {}", config::path().display());
            println!("cli folder: {}", cfg.cli_root().display());
            println!("allowed folders: {}", cfg.roots().join(", "));
            println!("\navailable apps:");
            for a in &cat.apps {
                let note = if cfg.exclude.contains(&a.id) { " (excluded)" } else { "" };
                println!("  {:<15} {:<28} {}{note}", a.id, a.repo, a.name);
            }
            for (id, why) in &cat.rejected {
                println!("  {id:<15} refused: {why}");
            }
            0
        }
    })
}

fn main() -> ExitCode {
    match real_main() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(n) => {
            eprintln!("{n} step(s) failed");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("craftctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}
