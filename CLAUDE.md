# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Control QQ音乐 (QQ Music) running on one Windows machine from a browser on another machine on the LAN. Server and web UI are a single process; the laptop installs nothing.

Read `README.md` first for user-facing behavior. It is deliberately a reference, not an essay: state what is, skip the why. Rationale belongs here instead — don't move it back.

## Commands

```sh
cargo build --release          # all five binaries
cargo clippy --all-targets     # must stay at zero warnings
cargo run --bin probe          # SMTC/audio-session diagnostics
cargo run --bin keyprobe       # which channel media keys travel on
node tests/render.mjs          # frontend render branches
```

What CI enforces on every push to `main` (`.github/workflows/ci.yml`, `windows-latest`) — run these before pushing:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
node tests/render.mjs
```

`tests/render.mjs` is the only automated test, and it covers the frontend only. Everything on the Rust side — SMTC transport, Core Audio volume, media-key injection, sleep suppression — can only be verified by running the real binaries against a live QQ音乐 instance. CI cannot: its runner has no player, no audio device, and is a non-interactive session where SMTC may not even initialize. A green CI does not mean the features work.

Releases are cut by pushing a `v*` tag (`.github/workflows/release.yml`). It checks the tag against `Cargo.toml`'s `version`, so bump that in the same commit as the tag. The zip's name deliberately carries no version — README links `/releases/latest/download/` with a fixed filename, so renaming the asset breaks that link.

## Binaries

| Binary | Role |
|---|---|
| `cross-next` | The server. Runs on the machine playing music. |
| `remote` | One-shot client. For mouse drivers that can bind "launch program". |
| `listen` | Resident forwarder. Grabs media keys via `RegisterHotKey`, forwards to the server. For drivers offering only preset media-key actions. Has no window or tray icon, so it takes over any previous instance on launch and stops via `--stop`. |
| `probe` | Prints session context, all SMTC sessions, all audio sessions. |
| `keyprobe` | Hooks keyboard/shell/hotkey channels at once to see what a driver actually emits. |

`remote` and `listen` are `windows_subsystem = "windows"` so they don't flash a console. Both call `AttachConsole(ATTACH_PARENT_PROCESS)` — launched from a shell they print, double-clicked they show a message box. Both declare Per-Monitor-v2 DPI awareness before creating any window; without it message boxes render blurry on high-DPI displays.

## Architecture

```
browser ──HTTP/JSON──▶ cross-next.exe
                       ├─ media thread (owns one MTA apartment)
                       │    ├─ SMTC SessionManager  → QQ音乐 transport
                       │    └─ Core Audio sessions  → QQ音乐 per-process volume
                       └─ HTTP thread-per-connection (embedded page + JSON API)
```

**One dedicated media thread** (`src/media.rs`) owns the `SessionManager`; HTTP threads talk to it over an `mpsc` channel and wait for a reply. This initializes COM once, keeps WinRT objects off other threads, and reuses a single manager instance — windows-rs [#2061](https://github.com/microsoft/windows-rs/issues/2061) leaks memory if you recreate it in a loop.

**No async runtime.** `src/winrt_block.rs` blocks on `IAsyncOperation` with `SetCompleted` + `Condvar`. windows-future 0.3 only exposes `IntoFuture`; its blocking `Async` trait is private. `block_on` **must** run on an MTA thread — blocking on STA deadlocks for want of a message pump.

**Only dependency is the `windows` crate.** HTTP (`src/http.rs`), the HTTP client (`src/client.rs`), and JSON in/out (`src/jsonlite.rs`) are hand-written. The JSON helpers do targeted extraction from known-shape single-level objects, not general parsing. Don't add axum/tokio/serde/reqwest without a concrete reason.

**Two control modes, auto-detected** (`media::Mode`). `smtc` when QQ音乐 registers an SMTC session: targeted control plus metadata. `mediakey` when it doesn't but Core Audio shows it playing: global `SendInput` media keys, no metadata at all. Real QQ音乐 builds differ on this — one machine registered SMTC with full metadata, another never did and only answered the old `WM_APPCOMMAND` path.

## Hard-won constraints

These came from running the code against real QQ音乐; don't re-derive them.

- **Session isolation.** SMTC sessions are per-Windows-logon-session. Non-interactive sessions see nothing; Win11 throws `0x80070424` from `RequestAsync()`. The server cannot be a service or started over SSH. `probe` checks this first via `ProcessIdToSessionId` vs `WTSGetActiveConsoleSessionId`.
- **Don't gate buttons on `Controls()` capability bits.** QQ音乐 reports them incompletely (`IsPauseEnabled=false` while status is `Opened`), which would grey out the play button forever. Send the command and report the `Try*Async` bool honestly — `false` means the app declined, which is not an error. This applies to **seeking** too, and the bit lies in both directions: QQ音乐 reports `IsPlaybackPositionEnabled=false` while `TryChangePlaybackPositionAsync` returns `true` and actually jumps (verified against a live player). `/api/state` still reports `canSeek` honestly, but the page ignores it for enabling the slider — `tests/render.mjs` has a case pinning that.
- **`Position` is a snapshot, not a live value.** It is only accurate as of `LastUpdatedTime`; while playing you must add the wall-clock delta since then, or a once-a-second poll returns the same stale number and the bar jumps a notch at a time. `read_timeline` does this **server-side** on purpose — the browser is on another machine, so using its clock would fold in the two machines' clock skew. `TimeSpan::Duration`, `DateTime::UniversalTime` and `FILETIME` all count 100ns ticks, and the latter two share the 1601 epoch, so they subtract directly.
- **`EndTime <= StartTime` means no timeline.** Live streams report it (Edge on a livestream gives `EndTime` of `-0.0`), as does a session that hasn't really started playing. Degrade `position`/`duration` to `null` so the page hides the bar rather than drawing nonsense. Note a session can still claim `IsPlaybackPositionEnabled=true` in this state.
- **Audio sessions are dynamic.** Core Audio lists only sessions that have been active, so QQ音乐 is absent when silent. Re-enumerate on every call; `volume: null` means "unavailable", not zero. Also: the API keys on PID, not name, and one app may hold several sessions — set all matches. `volume.rs` prefers exact process-name matches so a `target` of `qq` doesn't also hit `QQ.exe`.
- **Adapter choice.** Prefer adapters that have a default gateway. WSL2/Hyper-V virtual adapters use `172.x`, pass an RFC1918 check, and are unreachable from other LAN devices. Never bind `0.0.0.0`.
- **Thumbnails are not cached server-side.** SMTC art can lag the track metadata by a beat; caching pins the stale image permanently. The server returns a content hash as `ETag` with `Cache-Control: no-store`, and the page re-checks at 200ms/1.2s/3s after a track change.
- **Honor `Connection: close`.** `client.rs` sends it and reads exactly `Content-Length` bytes. A server that ignores the header while the client waits for EOF stalls until timeout.
- **There is no way to ask who owns a hotkey.** `RegisterHotKey` failing tells you only that it failed. So `listen` identifies *its own* prior instance instead: a hidden window with a unique class name, found via `FindWindowW` and asked to quit with `WM_CLOSE`. Deliberately not process-name matching plus a kill — `listen.exe` is a generic enough name to hit something unrelated, and `WM_CLOSE` lets the old instance `UnregisterHotKey` on its way out, which a kill does not. Its message loop must call `DispatchMessageW`; `WM_HOTKEY` goes to the thread and is handled inline, but without dispatch the marker window never sees `WM_CLOSE` and takeover silently does nothing.

## Frontend

`web/index.html` is a single file embedded via `include_str!` — rebuild after editing it. Immersive large-cover layout; cover art drives a `--a`/`--b` CSS variable gradient (`pickColors` skips near-greyscale pixels so dark covers don't average to mud). Polls `/api/state` at 1s, updates optimistically on click.

`tests/render.mjs` exercises every `render()` branch: it extracts the `<script>` block and `eval`s it under a stubbed DOM in Node, no dependencies. Add a case there when you add a branch. Three things that will bite you when editing it:

- The stub must grow with the page. Any new DOM API the page touches has to be stubbed or you get a fake failure — `document.documentElement` and element `addEventListener` were both missed on the first attempts.
- Not every user-visible bug lives in `render()`. The progress bar froze because `seekDragging` latched in a *pointer handler*, so calling `render()` alone could never catch it. The stub records listeners (`el.fire`, `__fireWindow`) and fakes timers and `performance.now` (`__runTimers`, `__advance`) so a test can drive the real event path. Reach for those before concluding something is untestable here.
- **Verify a new case actually fails without the fix.** Re-introduce the bug, watch it go red, then restore. Both progress-bar cases were confirmed this way; a case that passes either way is worse than none, because it looks like coverage.
- Test code must be appended to the *same* `eval` string. The page is strict-mode, so function declarations inside an `eval` stay scoped to it; a separate `eval` yields `render is not defined`.
- Regexes over `web/index.html` must tolerate CRLF. Git's `autocrlf` checks the file out with `\r\n` on Windows, so a hardcoded `\n` silently matches nothing.

This catches runtime `ReferenceError`s that `node --check` cannot — and those matter here, because an exception partway through `render()` silently kills every update after it (one such bug broke the play icon and the cover art at the same time).

## Conventions

Commit messages are in English. Comments and user-facing strings are in Chinese, matching README. Comments explain *why* — especially where behavior is counterintuitive or was settled by experiment. Keep that; several comments are the only record of a constraint.

`config.json` (server) and `remote.json` (client) live beside the exe, are generated on first run, hold the shared token, and are gitignored. Token comparison is constant-time. Plain HTTP by design — the token blocks fumbles from other devices on the subnet, not sniffing; the port must not be forwarded to the internet.
