# AGENTS.md

## Project purpose

An [OpenAction](https://openaction.amankhanna.me) plugin for [OpenDeck](https://github.com/nekename/OpenDeck) that shows [Claude Code](https://claude.com/claude-code) usage (session/weekly rate limits) on a stream-deck-style key.

The action renders a circular progress ring showing usage percentage for one of two windows (session = rolling 5-hour, weekly = rolling 7-day). Ring color is green below 50%, yellow below 85%, red 85-100%. Center text shows the time until the next usage reset.

## Architecture

- **Rust backend** (`src/main.rs`) using the official `openaction` crate v2.1. This is the recommended OpenAction language. Do not rewrite in another language unless asked.
- **Single action** with UUID `com.esteban-dcp.claudecodeusage.usage`, defined in `assets/manifest.json`.
- **Property inspector** (`assets/pi.html`): a webview that stores the selected `mode` and `show_percent` toggle in the action's settings via the OpenAction WebSocket (`setSettings` event). The backend reads them in `will_appear` / `did_receive_settings`.
- **Rendering**: the backend builds an SVG string and sends it to the key with `instance.set_image(...)` as a base64 data URI (`data:image/svg+xml;base64,...`). OpenDeck rasterises SVG at 144x144 in its frontend canvas, so SVG works on hardware. The manifest sets `ShowTitle: false` because the SVG carries all text. `show_percent` (default true) only hides the percentage `<text>`; font sizes and layout are fixed.
- **Manifest naming**: the plugin's manifest `Name` is `Claude Code Usage` (shown in OpenDeck's Plugin Manager and the marketplace catalogue; the catalogue `name` must match it exactly). The action's `Name` is `Usage` under the `Claude Code` category (shown in the actions list). `CategoryIcon` (`assets/claude.png`) and `assets/icon.svg` / `assets/actions/icon.svg` are a simple original terracotta (`#d97757`, Claude's brand color) spark mark — not Anthropic's logo. Keep the stage/install copies in sync if assets change.
- **Data source: there is no HTTP API or headless CLI command for Claude Code's usage percentage.** The only place Anthropic exposes `rate_limits` (`five_hour`, `seven_day`, each with `used_percentage` + `resets_at` epoch seconds) is the JSON Claude Code feeds to a **statusLine** script over stdin while an interactive session is open, only for Pro/Max/Team/Enterprise plans, and only after that session's first API response. There is no `claude usage --json` or equivalent (rejected upstream — the real endpoint backing `/usage` is authenticated with the session's stored OAuth token and shouldn't be called by third parties). See [statusline docs](https://code.claude.com/docs/en/statusline) for the full JSON schema.

## How the statusLine bridge works

- The same binary doubles as the statusLine script. When invoked as `oaclaudecode-usage --statusline-bridge`, `main()` short-circuits before starting tokio/the OpenAction runtime: it reads stdin synchronously, deserializes just the `rate_limits` field (`StatusLinePayload`/`RateLimits`/`RateWindow` in `src/main.rs`), merges it into the existing cache file (`merge_cache`, so a payload only reporting one window doesn't erase the other), and writes it back to `cache_file_path()` — `dirs::cache_dir()` joined with `oaclaudecode-usage/rate_limits.json`, falling back to `std::env::temp_dir()` if the OS cache dir can't be resolved. It also prints a short fallback line to stdout so the user's actual terminal status line stays useful.
- The normal plugin process (registered action + `tokio::spawn` ticker) never touches the network. It reads the same cache file (`read_cache` → `lookup_window`) on `will_appear`, `did_receive_settings`, `key_up`, and every 30-second tick, and renders whatever's there.
- A window is only usable if it's present in the cache **and** `resets_at` is still in the future; otherwise the key shows the "NO DATA — open Claude Code" state (`render_svg_waiting`) rather than a hard error. Actual read/parse failures (corrupt cache file) still render as an error SVG.
- Configuring `statusLine.command` in the user's `~/.claude/settings.json` to point at the installed binary is a manual, documented step (README) — this plugin does not edit files outside its own install directory.

## Key code conventions and gotchas

- **State tracking**: `INSTANCES` is a `static LazyLock<Mutex<HashMap<InstanceId, InstanceState>>>` keyed by `instance.instance_id`, updated in `will_appear` / `will_disappear` / `did_receive_settings`. The crate's own `Instance.settings_json` is `pub(crate)` and not readable from the plugin, so this map is how the ticker knows each instance's settings. Unlike the old opencode version, `InstanceState` no longer caches fetched values or a `last_fetch` timestamp — the cache file on disk is the only source of truth, and reading it is cheap local I/O, so every tick just re-reads it.
- **Do not hold the `INSTANCES` lock across `.await`**: `std::sync::MutexGuard` is not `Send`, so a guard held across an await fails to compile in spawned tasks and `async_trait` handlers. Snapshot (clone) state inside a scope, then await outside it.
- **SVG format strings**: use `r##"..."##` raw strings, never `r#"..."#`. The hex colors (`fill="#ffffff"`) contain the byte sequence `"#` which prematurely terminates `r#"..."#` and causes confusing `prefix ... is unknown` compile errors.
- **Truncate error text char-safely**: use `message.chars().take(16).collect()`, not `&message[..16]` (panics on multi-byte boundaries).
- **`--statusline-bridge` must stay fast and side-effect-minimal**: Claude Code invokes the statusLine command frequently during a session. `run_statusline_bridge()` must not initialize tokio, the logger, or the OpenAction runtime — it exits via `std::process::exit` before any of that in `main()`.
- **Pure helpers are testable**: `format_remaining`, `color_for`, `render_svg*`, `to_data_uri`, `merge_cache`, `lookup_window` are side-effect free. Add unit tests for any changes to them.
- Tabs for indentation (matches the `openaction` crate examples and this repo).

## Commands

```sh
cargo build --release     # build
cargo test --release      # unit tests (render/countdown/color/cache logic)
cargo clippy --release    # lint; must be clean before finishing
make stage                # assemble com.esteban-dcp.claudecodeusage.sdPlugin/
make package               # stage + zip com.esteban-dcp.claudecodeusage.zip
make install               # stage + copy into OpenDeck plugins dir
make clean                 # remove build artifacts, staged plugin dir, and package zip
```

OpenDeck plugins directory: Linux `~/.config/opendeck/plugins/` (or `$XDG_CONFIG_HOME/opendeck/plugins/`), macOS `~/Library/Application Support/OpenDeck/plugins/`.

## Distribution notes

- Manifest `CodePaths`/`CodePathWin`/`CodePathMac`/`CodePathLin` list the five standard platform triples (`x86_64`/`aarch64` × windows/mac/linux). The binary is named `oaclaudecode-usage-<target-triple>`.
- **Releases are PR-label driven** (modeled on `pd-slack`). Every PR must carry exactly one of the `major`/`minor`/`patch` labels; `.github/workflows/pr.yaml` validates this via the shared reusable `imdevinc/imdevinc/.github/workflows/shared-validate-semver-tags.yaml@v1`. Enforce it with branch protection requiring that check.
- On merge to `main`, `.github/workflows/build.yml` runs: `jefflinse/pr-semver-bump` (bump mode) derives the next version from the merged PR's label and tags it; the five triples are built natively; the bundle is assembled into `com.esteban-dcp.claudecodeusage.zip`. CI then writes the new version back into `Cargo.toml` and `assets/manifest.json` (committed to `main` as `github-actions[bot]`), force-updates the `vX.Y.Z` tag, and creates the GitHub Release with the zip. A merge without a release label fails the workflow and produces no release.
- Release tags must match the manifest `Version` (OpenDeck strips a leading `v` and compares). `Cargo.toml`/`assets/manifest.json` hold the seed version (`0.0.1`); after the first release the tag is the source of truth and CI keeps the files in sync.
- The release asset is named `<bundle-id>.zip` (`com.esteban-dcp.claudecodeusage.zip`), matching the convention used by other native plugins (e.g. `me.amankhanna.oadiscord.zip`).
- The plugin is not yet listed on the OpenAction Marketplace. When submitting, the catalogue entry key is the bundle id `com.esteban-dcp.claudecodeusage` in the "Native OpenAction plugins" section, with `name` = manifest `Name`, `author` = `esteban-dcp`, `repository` = this repo's URL, and an icon at `icons/com.esteban-dcp.claudecodeusage.png`. Submission is by contacting the maintainers (no fork).

## Docs maintenance

Keep `AGENTS.md` and `README.md` accurate and up to date automatically. When you change behavior, commands, dependencies, settings, or architecture, update the relevant section of both files in the same change. Do not wait to be asked.
