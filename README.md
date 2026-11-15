# craftctl

Install, update and wire up the [ArtCraft Crafting Apps](https://getartcraft.com/apps) on macOS
and Linux, and register each app's MCP server with Claude Code, Claude Desktop and Cursor, so an AI assistant
can drive FigCraft, CADCraft, PhotoCraft, SolidCraft and the rest.

> **Unofficial.** craftctl is not affiliated with or endorsed by ArtCraft. It installs only the
> official builds from each app's GitHub releases, and checks every one (see [Trust](#trust)).

Each app comes with a command-line tool (`<app>-cli`) that holds its MCP server; on macOS it is
a separate download from the GUI app. The ArtCraft Launcher handles the GUI apps but has no headless mode, and Homebrew
covers only a few of them and none of the CLIs. craftctl does all of it in one command, so it can
run from [topgrade](https://github.com/topgrade-rs/topgrade), cron or CI.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/badriram/craftctl/main/install.sh | sh
```

This installs the binary for your system (macOS universal, Linux x86_64 or aarch64) to
`~/.local/bin` and, if topgrade is installed, adds
`~/.config/topgrade.d/craftctl.toml` so every `topgrade` run updates the apps.
Or build it yourself: `cargo install --git https://github.com/badriram/craftctl`.

Then:

```sh
craftctl -n sync    # dry run: what would be installed and registered
craftctl sync       # install everything, register MCP servers
```

Restart Claude Desktop afterwards to load new servers.

## Commands

| Command | What it does |
|---|---|
| `craftctl status [app…]` | Installed GUI and CLI versions vs. the latest release |
| `craftctl update [app…]` | Install or update GUI apps and CLIs |
| `craftctl mcp [app…]` | Register MCP servers with the configured clients |
| `craftctl mcp --remove app…` | Unregister servers |
| `craftctl sync [app…]` | `update`, then `mcp` (what topgrade runs) |
| `craftctl config` | Config path, allowed folders, and every app the catalog offers |

`-n` / `--dry-run` works with all of them. With no app ids, every app not excluded is used.
An app that is open is skipped, and updated on the next run.

## Layout

macOS:

```
/Applications/<App>.app                  GUI apps (same place the ArtCraft Launcher uses)
~/Applications/artcraft/cli/<app>/       one current CLI per app
~/.local/bin/<app>-cli                   symlinks
```

Linux (per user, no root; from the release tarball, which holds both the app and its CLI):

```
~/.local/share/artcraft/apps/<app>/      the app's bin/ and share/
~/.local/bin/<app>, <app>-cli            symlinks
~/.local/share/applications/…desktop     menu entries, plus icons and file types in ~/.local/share
```

Both:

```
~/.config/craftctl/config.toml           settings (written, documented, on first run)
~/Documents/Craft/                       default folder agents may read and write
```

## Configuration

`~/.config/craftctl/config.toml` (or `$CRAFTCTL_CONFIG`):

```toml
exclude = []                                       # app ids to skip
install_apps = true                                # GUI apps / menu entries too, not just CLIs
mcp_targets = ["claude-code", "claude-desktop", "cursor"]
mcp_exclude = []                                   # install the CLI, but don't register it
allow_paths = ["~/Documents/Craft", "{claude_tmp}"] # folders the servers may use
# install_root = "~/Applications/artcraft"   # default; ~/.local/share/artcraft on Linux
bin_dir = "~/.local/bin"

[mcp_extra_args]
lightcraft = ["--compact"]   # ~400 tools otherwise
pdfcraft = ["--compact"]     # ~200 tools otherwise
```

`{claude_tmp}` is Claude Code's temp folder. Unknown keys are an error, so typos don't pass silently.

## MCP servers and file access

Each server is confined to `allow_paths` where the app supports it. The CLIs differ: most take
`--automation-read-root` / `--automation-write-root`, some accept only one folder of each kind,
SolidCraft uses `--allow-path`. craftctl detects which flags a binary has and tries the widest
form first, keeping the first that starts and answers an MCP `initialize`; the result is cached
per CLI version. Apps that accept a single folder get the first entry of `allow_paths`.

Some CLIs have no confinement option at all (currently CADCraft, GridCraft, LightCraft,
AniCraft, FilmCraft and Craft Anything). craftctl says so on every run; their servers can reach
whatever your user account can, so leave tool approval on in your MCP client, or list them in
`mcp_exclude`.

craftctl only touches MCP entries whose command lives in its install folder: it adds, updates
and prunes those, and leaves every other server alone. A client config is rewritten only when
something changed (Claude Desktop and Cursor keep a `.craftctl.bak` of the previous file). Claude Desktop's config
is `~/Library/Application Support/Claude/` on macOS and `~/.config/Claude/` on Linux.
Claude Code is updated through the `claude` CLI, so a running session never races a file write.

## Trust

- **Catalog.** The app list is ArtCraft's own launcher catalog, verified against the Ed25519 key
  pinned in the ArtCraft Launcher before it is parsed. Offline, the last verified copy is used.
- **Identity pinning.** A valid catalog still cannot redirect installs: official apps must come
  from `storytold/<id>` and be signed by ArtCraft's Apple team (`DJ6XS33FX8`). Community apps are
  installed only if pinned in the source (`src/catalog.rs`), with their repository and team;
  today that is SolidCraft (Tangency Labs). Adding one is a pull request, by design.
- **Downloads.** Every file must match the release's `SHA256SUMS.txt` and GitHub's own digest when it provides one.
- **Signatures, macOS.** App bundles must have a valid signature from the expected team and
  pass Gatekeeper as notarized; CLIs must have a valid, strict signature from the expected team.
- **Signatures, Linux.** Official tarballs must carry a valid OpenPGP signature from ArtCraft's
  Linux release key, pinned in `assets/` (fingerprint `2B76 741B 414A AD6C D33E 6312 13A0 05C0
  B9D7 C3ED`), checked by `gpg` against a throwaway keyring holding only that key. SolidCraft
  publishes no Linux signatures, so it gets checksum and repository pinning only, and craftctl
  says so when it installs it. Requires `gpg` (package `gnupg`).
- **Updates are atomic.** New versions are staged beside the old one and swapped in by rename.

A GitHub token is optional (it raises the API rate limit): `GITHUB_TOKEN`, `GH_TOKEN`, or the
`gh` CLI's login.

## Limits

- macOS and Linux. Windows (the releases have MSIs) is not supported yet; pull requests welcome.
- On Linux craftctl installs the tarball per user. Copies installed from the distribution's
  packages or Flatpak are left alone, and not detected.
- craftctl does not update itself. Re-run `install.sh`, or `cargo install` again.

## Development

```sh
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

The catalog tests use a real signed catalog from the launcher repository (`tests/fixtures`).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Includes the launcher's catalog key and a test fixture under Apache-2.0, and ArtCraft's public
Linux release key; see [NOTICE](NOTICE).
