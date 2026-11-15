//! The ArtCraft Launcher catalog: which apps exist and where they come from.
//!
//! The catalog is Ed25519-signed by ArtCraft; the signature is checked against the launcher's
//! pinned public key before parsing. A valid signature still cannot redirect installs:
//! official apps must live at `storytold/<id>` and be signed by ArtCraft's Apple team, and
//! community apps must match the repository and team pinned in `COMMUNITY` below. On Linux,
//! official tarballs must also be PGP-signed by ArtCraft's pinned release key.

use crate::net::Http;
use crate::util::home;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey, pkcs8::DecodePublicKey};
use serde::Deserialize;
use std::path::PathBuf;

pub const CATALOG_URL: &str = "https://getartcraft.com/launcher-catalog.toml";
pub const SIGNATURE_URL: &str = "https://getartcraft.com/launcher-catalog.toml.sig";
/// From storytold/craft-launcher, crates/core/assets/launcher-catalog-public-key.pem.
const PUBLIC_KEY_PEM: &str = include_str!("../assets/launcher-catalog-public-key.pem");
const MAX_CATALOG: u64 = 1 << 20;
const MAX_SIGNATURE: u64 = 128;

/// Apple Developer team that signs every official ArtCraft app.
pub const ARTCRAFT_TEAM: &str = "DJ6XS33FX8";

/// Community apps craftctl will install: (id, GitHub repository, Apple team, Linux PGP key).
/// Adding one is a code change on purpose, as it is in the launcher.
pub const COMMUNITY: &[(&str, &str, &str, Option<&str>)] =
    &[("solidcraft", "tangency-labs/solidcraft", "G5QA5U5X2S", None)];

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
    /// CLI is `<binary>-cli`.
    pub binary: String,
    /// GUI bundle is `/Applications/<bundle_name>.app`.
    pub bundle_name: String,
    pub repo: String,
    pub team: String,
    /// Fingerprint of the key that signs Linux releases, if the publisher signs them.
    pub linux_key: Option<String>,
}

#[derive(Debug)]
pub struct Catalog {
    pub apps: Vec<App>,
    /// Rows refused by the identity policy: (id, reason).
    pub rejected: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct Record {
    id: String,
    name: String,
    #[serde(default)]
    binary: Option<String>,
    #[serde(default)]
    mac_bundle_name: Option<String>,
    #[serde(default)]
    mac_team: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct Document {
    apps: Vec<toml::Value>,
}

pub fn pinned_key() -> VerifyingKey {
    VerifyingKey::from_public_key_pem(PUBLIC_KEY_PEM).expect("compiled catalog key is valid")
}

pub fn verify(bytes: &[u8], signature: &[u8], key: &VerifyingKey) -> Result<()> {
    let encoded = std::str::from_utf8(signature).context("signature is not UTF-8")?.trim();
    let raw = STANDARD.decode(encoded).context("signature is not base64")?;
    let sig = Signature::from_slice(&raw).map_err(|_| anyhow!("signature is not 64 bytes"))?;
    key.verify_strict(bytes, &sig)
        .map_err(|_| anyhow!("catalog signature does not match ArtCraft's pinned key"))
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Apply the identity policy to one catalog row.
fn vet(r: Record) -> Result<App, String> {
    if !safe_name(&r.id) {
        return Err("unsafe app id".into());
    }
    let repo = r.repo.unwrap_or_default();
    let (team, linux_key) = if r.kind.as_deref() == Some("official") {
        if repo != format!("storytold/{}", r.id) {
            return Err(format!("official app outside storytold/{}", r.id));
        }
        if r.mac_team.as_deref().is_some_and(|t| t != ARTCRAFT_TEAM) {
            return Err("official app with a non-ArtCraft signing team".into());
        }
        (ARTCRAFT_TEAM.to_string(), Some(crate::pgp::ARTCRAFT_FPR.to_string()))
    } else {
        match COMMUNITY.iter().find(|(id, ..)| *id == r.id) {
            Some((_, pinned_repo, pinned_team, key)) if repo == *pinned_repo => {
                (pinned_team.to_string(), key.map(String::from))
            }
            Some(_) => return Err("repository differs from the pinned one".into()),
            None => return Err("community app not in craftctl's pinned allowlist".into()),
        }
    };
    let binary = r.binary.unwrap_or_else(|| r.id.clone());
    if !safe_name(&binary) {
        return Err("unsafe binary name".into());
    }
    let bundle_name = r.mac_bundle_name.unwrap_or_else(|| r.name.clone());
    if bundle_name.is_empty() || bundle_name.contains('/') || bundle_name.starts_with('.') {
        return Err("unsafe bundle name".into());
    }
    Ok(App { id: r.id, name: r.name, binary, bundle_name, repo, team, linux_key })
}

/// Parse verified catalog bytes. Bad rows are skipped, not fatal.
pub fn parse(bytes: &[u8]) -> Result<Catalog> {
    let text = std::str::from_utf8(bytes).context("catalog is not UTF-8")?;
    let doc: Document = toml::from_str(text).context("parsing catalog")?;
    let mut cat = Catalog { apps: vec![], rejected: vec![] };
    for row in doc.apps {
        let id = row.get("id").and_then(|v| v.as_str()).unwrap_or("?").to_string();
        match row.try_into::<Record>() {
            Ok(rec) => match vet(rec) {
                Ok(app) => cat.apps.push(app),
                Err(why) => cat.rejected.push((id, why)),
            },
            Err(e) => cat.rejected.push((id, format!("malformed row: {e}"))),
        }
    }
    if cat.apps.is_empty() {
        bail!("catalog has no usable apps");
    }
    Ok(cat)
}

fn cache_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        return home().join("Library/Caches/craftctl");
    }
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("craftctl"),
        _ => home().join(".cache/craftctl"),
    }
}

/// Fetch, verify and parse the catalog; fall back to the last verified copy when offline.
pub fn load(http: &Http) -> Result<Catalog> {
    let key = pinned_key();
    let fresh = http
        .get_bytes(CATALOG_URL, MAX_CATALOG)
        .and_then(|b| Ok((b, http.get_bytes(SIGNATURE_URL, MAX_SIGNATURE)?)))
        .and_then(|(b, s)| verify(&b, &s, &key).map(|_| (b, s)));
    let (dir, toml_path, sig_path) = {
        let d = cache_dir();
        (d.clone(), d.join("launcher-catalog.toml"), d.join("launcher-catalog.toml.sig"))
    };
    let bytes = match fresh {
        Ok((b, s)) => {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(&toml_path, &b);
            let _ = std::fs::write(&sig_path, &s);
            b
        }
        Err(e) => {
            eprintln!(
                "warning: catalog unavailable ({}); using the cached copy",
                crate::util::first_line(&e)
            );
            let b = std::fs::read(&toml_path).context("no cached catalog either")?;
            verify(&b, &std::fs::read(&sig_path)?, &key).context("cached catalog")?;
            b
        }
    };
    parse(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/hosted-catalog.toml");
    const FIXTURE_SIG: &[u8] = include_bytes!("../tests/fixtures/hosted-catalog.toml.sig");

    #[test]
    fn real_signed_catalog_verifies_with_pinned_key() {
        verify(FIXTURE, FIXTURE_SIG, &pinned_key()).unwrap();
        let cat = parse(FIXTURE).unwrap();
        assert!(cat.apps.iter().any(|a| a.id == "figcraft" && a.repo == "storytold/figcraft"));
    }

    #[test]
    fn tampered_catalog_is_rejected() {
        let mut bad = FIXTURE.to_vec();
        bad[10] ^= 1;
        assert!(verify(&bad, FIXTURE_SIG, &pinned_key()).is_err());
    }

    fn row(extra: &str) -> Vec<u8> {
        format!("[[apps]]\nname = \"X\"\nmac_bundle_name = \"X\"\n{extra}\n").into_bytes()
    }

    #[test]
    fn policy_pins_official_and_community_identities() {
        let ok =
            parse(&row("id = \"figcraft\"\nkind = \"official\"\nrepo = \"storytold/figcraft\"")).unwrap();
        assert_eq!(ok.apps[0].team, ARTCRAFT_TEAM);

        let moved = row("id = \"figcraft\"\nkind = \"official\"\nrepo = \"evil/figcraft\"");
        assert!(parse(&moved).is_err());

        let team =
            row("id = \"figcraft\"\nkind = \"official\"\nrepo = \"storytold/figcraft\"\nmac_team = \"EVIL\"");
        assert!(parse(&team).is_err());

        let solid =
            parse(&row("id = \"solidcraft\"\nkind = \"community\"\nrepo = \"tangency-labs/solidcraft\""))
                .unwrap();
        assert_eq!(solid.apps[0].team, "G5QA5U5X2S");
        assert_eq!(solid.apps[0].linux_key, None);
        assert_eq!(ok.apps[0].linux_key.as_deref(), Some(crate::pgp::ARTCRAFT_FPR));

        let unknown = row("id = \"mailcraft\"\nkind = \"community\"\nrepo = \"someone/mailcraft\"");
        assert!(parse(&unknown).is_err());

        let traversal = row("id = \"../x\"\nkind = \"official\"\nrepo = \"storytold/../x\"");
        assert!(parse(&traversal).is_err());
    }
}
