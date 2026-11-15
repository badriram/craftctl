//! Registering MCP servers with clients: Claude Code, Claude Desktop, Cursor.
//!
//! craftctl owns only entries whose command lives under its CLI folder ("managed" entries):
//! it adds and updates the servers it is asked for, prunes managed entries for apps that are
//! no longer wanted, and leaves everything else alone. Nothing is written when nothing changed.

use crate::util::{home, run, which};
use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Add(Server),
    Update(Server),
    Remove(String),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Change::Add(s) => write!(f, "add {}", s.name),
            Change::Update(s) => write!(f, "update {}", s.name),
            Change::Remove(n) => write!(f, "remove {n}"),
        }
    }
}

/// What to do: servers to ensure, managed names to remove, and whether to prune
/// managed entries that are not in `desired` (only on a full run).
pub struct Plan<'a> {
    pub desired: &'a [Server],
    pub remove: &'a [String],
    pub prune: bool,
    /// Commands under this folder are craftctl's.
    pub managed_prefix: &'a Path,
}

fn is_managed(entry: &Value, prefix: &Path) -> bool {
    entry.get("command").and_then(Value::as_str).is_some_and(|c| Path::new(c).starts_with(prefix))
}

/// Apply a plan to an `mcpServers` map, returning what changed. `shape` builds a new entry.
pub fn merge(servers: &mut Map<String, Value>, plan: &Plan, shape: fn(&Server) -> Value) -> Vec<Change> {
    let mut changes = vec![];
    for s in plan.desired {
        let same = servers.get(&s.name).is_some_and(|cur| {
            cur.get("command") == Some(&json!(s.command)) && cur.get("args") == Some(&json!(s.args))
        });
        if same {
            continue;
        }
        match servers.get_mut(&s.name) {
            Some(Value::Object(cur)) => {
                cur.insert("command".into(), json!(s.command));
                cur.insert("args".into(), json!(s.args));
                changes.push(Change::Update(s.clone()));
            }
            _ => {
                let existed = servers.contains_key(&s.name);
                servers.insert(s.name.clone(), shape(s));
                changes.push(if existed { Change::Update(s.clone()) } else { Change::Add(s.clone()) });
            }
        }
    }
    let wanted: Vec<&str> = plan.desired.iter().map(|s| s.name.as_str()).collect();
    let doomed: Vec<String> = servers
        .iter()
        .filter(|(name, entry)| {
            is_managed(entry, plan.managed_prefix)
                && (plan.remove.contains(name) || (plan.prune && !wanted.contains(&name.as_str())))
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in doomed {
        servers.remove(&name);
        changes.push(Change::Remove(name));
    }
    changes
}

fn file_shape(s: &Server) -> Value {
    json!({ "command": s.command, "args": s.args })
}

fn claude_code_shape(s: &Server) -> Value {
    json!({ "type": "stdio", "command": s.command, "args": s.args, "env": {} })
}

/// JSON-file clients (Claude Desktop, Cursor). Skipped when the client is not installed.
fn json_file_client(label: &str, path: &Path, plan: &Plan, dry_run: bool) -> Result<Vec<String>> {
    if !path.parent().is_some_and(Path::is_dir) {
        return Ok(vec![format!("{label}: not installed, skipped")]);
    }
    let mut doc: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?,
        Err(_) => json!({}),
    };
    let root = doc.as_object_mut().with_context(|| format!("{} is not a JSON object", path.display()))?;
    let servers = root.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers.as_object_mut().context("mcpServers is not an object")?;
    let changes = merge(servers, plan, file_shape);
    let mut log: Vec<String> = changes.iter().map(|c| format!("{label}: {c}")).collect();
    if changes.is_empty() {
        log.push(format!("{label}: up to date"));
    } else if !dry_run {
        if path.exists() {
            std::fs::copy(path, path.with_extension("json.craftctl.bak"))?;
        }
        let tmp = path.with_extension("json.craftctl-tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&doc)? + "\n")?;
        std::fs::rename(&tmp, path)?;
        if label == "claude-desktop" {
            log.push("claude-desktop: restart Claude Desktop to load the changes".into());
        }
    }
    Ok(log)
}

fn claude_json() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d).join(".claude.json"),
        None => home().join(".claude.json"),
    }
}

/// Claude Code: diff against its user-scope config, apply through the `claude` CLI so a
/// running session never races a direct file write.
fn claude_code(plan: &Plan, dry_run: bool) -> Result<Vec<String>> {
    let Some(claude) = which("claude") else {
        return Ok(vec!["claude-code: not installed, skipped".into()]);
    };
    let mut current = std::fs::read_to_string(claude_json())
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("mcpServers").and_then(Value::as_object).cloned())
        .unwrap_or_default();
    let changes = merge(&mut current, plan, claude_code_shape);
    let mut log = vec![];
    for c in &changes {
        let mut line = format!("claude-code: {c}");
        if !dry_run {
            let name = match c {
                Change::Add(s) | Change::Update(s) => &s.name,
                Change::Remove(n) => n,
            };
            let _ = Command::new(&claude).args(["mcp", "remove", "-s", "user", name]).output();
            if let Change::Add(s) | Change::Update(s) = c {
                let spec = claude_code_shape(s).to_string();
                if let Err(e) =
                    run(Command::new(&claude).args(["mcp", "add-json", "-s", "user", name, &spec]))
                {
                    line.push_str(&format!(" FAILED: {e}"));
                }
            }
        }
        log.push(line);
    }
    if changes.is_empty() {
        log.push("claude-code: up to date".into());
    }
    Ok(log)
}

pub fn apply(targets: &[String], plan: &Plan, dry_run: bool) -> Vec<String> {
    let mut log = vec![];
    for t in targets {
        let result = match t.as_str() {
            "claude-code" => claude_code(plan, dry_run),
            "claude-desktop" => json_file_client(
                t,
                &home().join("Library/Application Support/Claude/claude_desktop_config.json"),
                plan,
                dry_run,
            ),
            "cursor" => json_file_client(t, &home().join(".cursor/mcp.json"), plan, dry_run),
            other => Ok(vec![format!("{other}: unknown MCP client, skipped")]),
        };
        match result {
            Ok(lines) => log.extend(lines),
            Err(e) => log.push(format!("{t}: ERROR {e:#}")),
        }
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, cmd: &str) -> Server {
        Server { name: name.into(), command: cmd.into(), args: vec!["mcp".into()] }
    }

    #[test]
    fn merge_adds_updates_prunes_and_keeps_foreign_entries() {
        let prefix = Path::new("/m/cli");
        let mut servers: Map<String, Value> = serde_json::from_value(json!({
            "rea": { "command": "npx", "args": ["rea"] },
            "figcraft": { "command": "/old/figcraft-cli", "args": ["mcp"], "env": { "K": "V" } },
            "gone": { "command": "/m/cli/gone/gone-cli", "args": ["mcp"] },
            "same": { "command": "/m/cli/same/same-cli", "args": ["mcp"] },
        }))
        .unwrap();
        let desired = [
            server("figcraft", "/m/cli/figcraft/figcraft-cli"),
            server("same", "/m/cli/same/same-cli"),
            server("new", "/m/cli/new/new-cli"),
        ];
        let plan = Plan { desired: &desired, remove: &[], prune: true, managed_prefix: prefix };
        let changes = merge(&mut servers, &plan, file_shape);
        assert_eq!(
            changes,
            vec![
                Change::Update(desired[0].clone()),
                Change::Add(desired[2].clone()),
                Change::Remove("gone".into())
            ]
        );
        assert!(servers.contains_key("rea"), "foreign entries are kept");
        assert_eq!(servers["figcraft"]["env"]["K"], "V", "extra keys survive an update");
        assert!(merge(&mut servers, &plan, file_shape).is_empty(), "second run is a no-op");
    }

    #[test]
    fn partial_runs_do_not_prune_and_remove_only_touches_managed() {
        let prefix = Path::new("/m/cli");
        let mut servers: Map<String, Value> = serde_json::from_value(json!({
            "a": { "command": "/m/cli/a/a-cli", "args": ["mcp"] },
            "mine": { "command": "/usr/local/bin/x", "args": [] },
        }))
        .unwrap();
        let none: [Server; 0] = [];
        let plan = Plan { desired: &none, remove: &[], prune: false, managed_prefix: prefix };
        assert!(merge(&mut servers, &plan, file_shape).is_empty());
        let rm = ["a".to_string(), "mine".to_string()];
        let plan = Plan { desired: &none, remove: &rm, prune: false, managed_prefix: prefix };
        assert_eq!(merge(&mut servers, &plan, file_shape), vec![Change::Remove("a".into())]);
        assert!(servers.contains_key("mine"));
    }
}
