//! OpenPGP checks for Linux release files, through `gpg` with a throwaway keyring that holds
//! only the pinned publisher key.

use crate::util::{run, which};
use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// "ArtCraft Linux Release Signing Key", published as `release-signing-key.asc` in every
/// official release. craftctl pins it rather than trusting the copy beside the files.
pub const ARTCRAFT_FPR: &str = "2B76741B414AAD6CD33E631213A005C0B9D7C3ED";
const ARTCRAFT_KEY: &str = include_str!("../assets/artcraft-linux-release-key.asc");

fn key_for(fpr: &str) -> Option<&'static str> {
    (fpr == ARTCRAFT_FPR).then_some(ARTCRAFT_KEY)
}

/// The primary-key fingerprints `gpg --status-fd` reports as valid signatures.
/// VALIDSIG <signing-key-fpr> <date> <ts> <expiry> <ver> <reserved> <algo> <hash> <class> <primary-fpr>
pub fn valid_signers(status: &str) -> Vec<String> {
    status
        .lines()
        .filter_map(|l| l.strip_prefix("[GNUPG:] VALIDSIG "))
        .filter_map(|rest| rest.split_whitespace().last().map(str::to_ascii_uppercase))
        .collect()
}

/// Check `sig` is a valid detached signature over `file` by the key with fingerprint `fpr`.
pub fn verify_detached(file: &Path, sig: &Path, fpr: &str) -> Result<()> {
    let key = key_for(fpr).with_context(|| format!("no pinned key for {fpr}"))?;
    let gpg = which("gpg").context("gpg is needed to check ArtCraft's Linux signatures (install gnupg)")?;
    let home = tempfile::Builder::new().prefix("craftctl-gpg-").tempdir()?;
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700))?;
    let key_file = home.path().join("key.asc");
    std::fs::write(&key_file, key)?;
    let base = |c: &mut Command| {
        c.arg("--homedir").arg(home.path()).args(["--batch", "--no-tty", "--quiet"]);
    };
    let mut import = Command::new(&gpg);
    base(&mut import);
    run(import.arg("--import").arg(&key_file))?;
    let mut verify = Command::new(&gpg);
    base(&mut verify);
    let out = verify.args(["--status-fd", "1", "--verify"]).arg(sig).arg(file).output()?;
    let signers = valid_signers(&String::from_utf8_lossy(&out.stdout));
    if !out.status.success() || !signers.iter().any(|s| s == fpr) {
        bail!("{} has no valid signature from the pinned key {fpr}", file.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_validsig_primary_fingerprint() {
        let status = "[GNUPG:] NEWSIG\n[GNUPG:] VALIDSIG 568DC1F68F8FAECC797F3C7B006DAF6BB0EF334E 2026-11-10 \
                      1794700000 0 4 0 22 10 00 2b76741b414aad6cd33e631213a005c0b9d7c3ed\n";
        assert_eq!(valid_signers(status), [ARTCRAFT_FPR]);
        assert!(valid_signers("[GNUPG:] BADSIG 1234 x\n").is_empty());
    }

    #[test]
    fn pinned_key_has_the_pinned_fingerprint() {
        let Some(gpg) = which("gpg") else { return };
        let out = Command::new(gpg)
            .args(["--batch", "--with-colons", "--show-keys"])
            .arg(std::env::current_dir().unwrap().join("assets/artcraft-linux-release-key.asc"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let first_fpr = text.lines().find(|l| l.starts_with("fpr:")).and_then(|l| l.split(':').nth(9));
        assert_eq!(first_fpr, Some(ARTCRAFT_FPR));
    }
}
