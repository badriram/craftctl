//! HTTPS: the signed catalog, GitHub release metadata and checksummed downloads.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub struct Http {
    agent: ureq::Agent,
    token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    /// GitHub's own "sha256:<hex>" digest, when the API provides it.
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// A GitHub token raises the API rate limit (60/hour anonymous). Optional.
fn token() -> Option<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(t) = std::env::var(var)
            && !t.trim().is_empty()
        {
            return Some(t.trim().to_string());
        }
    }
    crate::util::which("gh")?;
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    let t = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

impl Http {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .user_agent(concat!("craftctl/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Http { agent, token: token() }
    }

    pub fn get_bytes(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
        let mut resp = self.agent.get(url).call().with_context(|| format!("GET {url}"))?;
        let body = resp.body_mut().with_config().limit(limit).read_to_vec()?;
        Ok(body)
    }

    pub fn latest_release(&self, repo: &str) -> Result<Release> {
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        let mut req = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = &self.token {
            req = req.header("Authorization", &format!("Bearer {t}"));
        }
        let mut resp = req.call().with_context(|| format!("GitHub release lookup for {repo}"))?;
        let body = resp.body_mut().with_config().limit(16 << 20).read_to_vec()?;
        let api: ApiRelease = serde_json::from_slice(&body).context("parsing GitHub release")?;
        Ok(Release {
            version: api.tag_name.trim_start_matches('v').to_string(),
            tag: api.tag_name,
            assets: api.assets,
        })
    }

    /// Stream `url` to `dest`, returning the SHA-256 of what was written.
    pub fn download(&self, url: &str, dest: &Path) -> Result<String> {
        let resp = self.agent.get(url).call().with_context(|| format!("GET {url}"))?;
        let mut reader = resp.into_body().into_with_config().limit(8 << 30).reader();
        let mut file = std::fs::File::create(dest)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
        }
        file.sync_all()?;
        Ok(hex(&hasher.finalize()))
    }

    /// Download a release asset and check it against the release's SHA256SUMS.txt
    /// (and GitHub's own digest when present).
    pub fn fetch_verified(&self, rel: &Release, name: &str, dir: &Path) -> Result<std::path::PathBuf> {
        let asset = rel.asset(name).with_context(|| format!("{name} is not in release {}", rel.tag))?;
        let sums = rel.asset("SHA256SUMS.txt").context("release has no SHA256SUMS.txt")?;
        let sums = String::from_utf8(self.get_bytes(&sums.url, 1 << 20)?)?;
        let expected = parse_sha256sums(&sums)
            .remove(name)
            .with_context(|| format!("{name} is not listed in SHA256SUMS.txt"))?;
        let path = dir.join(name);
        let got = self.download(&asset.url, &path)?;
        if got != expected {
            bail!("checksum mismatch for {name}");
        }
        if let Some(d) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:"))
            && !d.eq_ignore_ascii_case(&got)
        {
            bail!("{name} does not match GitHub's recorded digest");
        }
        Ok(path)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `sha256sum` format: "<hex>  <name>" or "<hex> *<name>".
pub fn parse_sha256sums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (hash, name) = (parts.next()?, parts.next()?);
            (parts.next().is_none() && hash.len() == 64)
                .then(|| (name.trim_start_matches('*').to_string(), hash.to_ascii_lowercase()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sha256sums() {
        let h = "a".repeat(64);
        let text = format!("{h}  app.dmg\n{h} *cli.zip\ngarbage\n{}  short\n", "b".repeat(10));
        let m = parse_sha256sums(&text);
        assert_eq!(m.len(), 2);
        assert_eq!(m["app.dmg"], h);
        assert_eq!(m["cli.zip"], h);
    }

    #[test]
    fn hex_encodes() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }
}
