//! Working out each CLI's `mcp` arguments.
//!
//! The CLIs confine file access differently, and their help text is not uniform, so instead of
//! hard-coding per-app knowledge craftctl detects which confinement flags a binary contains
//! and, for the automation-root style, tries the widest form first (many read and write roots),
//! then fewer, keeping the first that starts and answers an MCP `initialize`. Results are
//! cached in the CLI folder, which an update replaces, so each version is probed once.

use crate::catalog::App;
use crate::config::Config;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const FLAGS: [&str; 2] = ["automation-write-root", "allow-path"];
const FLAGS_CACHE: &str = ".mcp-flags";
const ROOTS_CACHE: &str = ".mcp-roots";

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Confinement {
    /// `--automation-read-root` / `--automation-write-root`.
    Roots,
    /// SolidCraft's `--allow-path` + `--require-allow-path`.
    AllowPath,
    /// The CLI has no way to confine file access.
    None,
}

fn confine_flags(bin: &Path) -> Vec<String> {
    let cache = bin.with_file_name(FLAGS_CACHE);
    if let Ok(s) = std::fs::read_to_string(&cache) {
        return s.split_whitespace().map(String::from).collect();
    }
    let Ok(bytes) = std::fs::read(bin) else { return vec![] };
    let found: Vec<String> = FLAGS
        .iter()
        .filter(|f| memchr::memmem::find(&bytes, f.as_bytes()).is_some())
        .map(|f| f.to_string())
        .collect();
    let _ = std::fs::write(&cache, found.join(" ") + "\n");
    found
}

/// Read and write roots are separate grants: a kind left out grants nothing.
pub fn root_args(roots: &[String], read_multi: bool, write_multi: bool) -> Vec<String> {
    let take = |multi: bool| if multi { roots } else { &roots[..roots.len().min(1)] };
    let mut args = vec![];
    for p in take(read_multi) {
        args.extend(["--automation-read-root".to_string(), p.clone()]);
    }
    for p in take(write_multi) {
        args.extend(["--automation-write-root".to_string(), p.clone()]);
    }
    args
}

/// True if the server starts with these arguments and answers an MCP initialize.
pub fn answers_initialize(bin: &Path, args: &[String]) -> bool {
    let Ok(mut child) = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"craftctl-probe","version":"1"}}}"#;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut ok = false;
    if writeln!(stdin, "{init}").and_then(|_| stdin.flush()).is_ok() {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(line) = rx.recv_timeout(left) else { break };
            if let Ok(msg) = serde_json::from_str::<Value>(&line)
                && msg.get("id") == Some(&Value::from(1))
            {
                ok = msg.get("result").is_some();
                break;
            }
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    ok
}

/// The `mcp` arguments for an app's CLI, and how it is confined.
pub fn mcp_args(cfg: &Config, app: &App, bin: &Path) -> (Vec<String>, Confinement) {
    let roots = cfg.roots();
    let mut base = vec!["mcp".to_string()];
    base.extend(cfg.extra_args(&app.id).iter().cloned());
    let flags = confine_flags(bin);
    if roots.is_empty() {
        return (base, Confinement::None);
    }
    if flags.iter().any(|f| f == "automation-write-root") {
        let cache = bin.with_file_name(ROOTS_CACHE);
        let cached = std::fs::read_to_string(&cache).ok().and_then(|s| {
            let mut w = s.split_whitespace();
            Some((w.next()? == "multi", w.next()? == "multi"))
        });
        let modes = match cached {
            Some(m) => vec![m],
            None => vec![(true, true), (true, false), (false, false)],
        };
        for (r, w) in modes {
            let mut args = base.clone();
            args.extend(root_args(&roots, r, w));
            if cached.is_some() || answers_initialize(bin, &args) {
                let word = |m: bool| if m { "multi" } else { "single" };
                let _ = std::fs::write(&cache, format!("{} {}\n", word(r), word(w)));
                return (args, Confinement::Roots);
            }
        }
        return (base, Confinement::None);
    }
    if flags.iter().any(|f| f == "allow-path") {
        let mut args = base;
        for p in &roots {
            args.extend(["--allow-path".to_string(), p.clone()]);
        }
        args.push("--require-allow-path".into());
        return (args, Confinement::AllowPath);
    }
    (base, Confinement::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_args_respects_single_and_multi() {
        let roots = vec!["/a".to_string(), "/b".to_string()];
        assert_eq!(
            root_args(&roots, true, false),
            ["--automation-read-root", "/a", "--automation-read-root", "/b", "--automation-write-root", "/a"]
        );
        assert_eq!(root_args(&roots, false, false).len(), 4);
        assert_eq!(root_args(&roots, true, true).len(), 8);
        assert!(root_args(&[], true, true).is_empty());
    }

    #[test]
    fn probe_fails_for_a_program_that_is_not_a_server() {
        assert!(!answers_initialize(Path::new("/usr/bin/true"), &[]));
    }
}
