//! User configuration: `~/.config/craftctl/config.toml` (or `$CRAFTCTL_CONFIG`).

use crate::util::{expand, home};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Written on first run, so every setting is visible and documented.
pub const DEFAULT_TOML: &str = r#"# craftctl configuration. App ids come from the ArtCraft catalog: run `craftctl status`.

# Apps to skip entirely.
exclude = []

# Install / update the GUI .app bundles in /Applications as well as the CLIs.
install_apps = true

# MCP clients to register servers with: "claude-code", "claude-desktop", "cursor".
# A client that is not installed is skipped.
mcp_targets = ["claude-code", "claude-desktop", "cursor"]

# Apps whose MCP server should not be registered (the CLI is still installed).
mcp_exclude = []

# Folders the MCP servers may read and write; the apps refuse anything else.
# Apps that accept a single folder get the FIRST entry. Missing folders are skipped,
# except the first, which is created. {claude_tmp} is Claude Code's temp folder.
allow_paths = ["~/Documents/Craft", "{claude_tmp}"]

# Where CLIs are kept, and where their symlinks go (should be on PATH).
install_root = "~/Applications/artcraft"
bin_dir = "~/.local/bin"

# Extra arguments for a server's `mcp` command, per app id.
[mcp_extra_args]
lightcraft = ["--compact"]   # ~400 tools otherwise
pdfcraft = ["--compact"]     # ~200 tools otherwise
"#;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub exclude: Vec<String>,
    pub install_apps: bool,
    pub mcp_targets: Vec<String>,
    pub mcp_exclude: Vec<String>,
    pub allow_paths: Vec<String>,
    pub install_root: String,
    pub bin_dir: String,
    pub mcp_extra_args: BTreeMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        Config {
            exclude: vec![],
            install_apps: true,
            mcp_targets: s(&["claude-code", "claude-desktop", "cursor"]),
            mcp_exclude: vec![],
            allow_paths: s(&["~/Documents/Craft", "{claude_tmp}"]),
            install_root: "~/Applications/artcraft".into(),
            bin_dir: "~/.local/bin".into(),
            mcp_extra_args: BTreeMap::from([
                ("lightcraft".into(), s(&["--compact"])),
                ("pdfcraft".into(), s(&["--compact"])),
            ]),
        }
    }
}

pub fn path() -> PathBuf {
    match std::env::var_os("CRAFTCTL_CONFIG") {
        Some(p) => PathBuf::from(p),
        None => home().join(".config/craftctl/config.toml"),
    }
}

/// Load the config, writing the documented default on first run.
pub fn load() -> Result<Config> {
    let p = path();
    if !p.exists() {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, DEFAULT_TOML).with_context(|| format!("writing {}", p.display()))?;
    }
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))
}

impl Config {
    pub fn cli_root(&self) -> PathBuf {
        expand(&self.install_root).join("cli")
    }

    pub fn bin_dir(&self) -> PathBuf {
        expand(&self.bin_dir)
    }

    /// Allowed folders that exist, in order (the first one is created if missing).
    pub fn roots(&self) -> Vec<String> {
        if let Some(first) = self.allow_paths.first() {
            let p = expand(first);
            if p.starts_with(home()) && !p.exists() {
                let _ = std::fs::create_dir_all(&p);
            }
        }
        self.allow_paths
            .iter()
            .map(|p| expand(p))
            .filter(|p| p.is_dir())
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    pub fn extra_args(&self, id: &str) -> &[String] {
        self.mcp_extra_args.get(id).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toml_matches_default_struct() {
        let parsed: Config = toml::from_str(DEFAULT_TOML).unwrap();
        let def = Config::default();
        assert_eq!(toml::to_string(&parsed).unwrap(), toml::to_string(&def).unwrap());
    }

    #[test]
    fn missing_keys_take_defaults_and_typos_fail() {
        let c: Config = toml::from_str("exclude = [\"mailcraft\"]").unwrap();
        assert_eq!(c.exclude, vec!["mailcraft"]);
        assert_eq!(c.extra_args("lightcraft"), ["--compact"]);
        assert!(toml::from_str::<Config>("exlude = []").is_err());
    }
}
