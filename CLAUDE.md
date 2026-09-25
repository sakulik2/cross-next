# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Control QQ音乐 (QQ Music) running on one Windows machine from a browser on another machine on the LAN. Server and web UI are a single process; the laptop installs nothing.

Read `README.md` first for user-facing behavior. It is deliberately a reference, not an essay: state what is, skip the why. Rationale belongs here instead — don't move it back.

## Commands

```sh
cargo build --release          # all five binaries
cargo clippy --all-targets     # must stay at zero warnings
cargo test                     # Rust-side pure-logic units (ICO parsing)
cargo run --bin probe          # SMTC/audio-session diagnostics
cargo run --bin keyprobe       # which channel media keys travel on
node tests/render.mjs          # frontend render branches
node tools/make-icon.mjs       # regenerate assets/tray.ico, prints ASCII preview
```

`assets/tray.ico` is a generated artifact that is committed, because `tray.rs` embeds it with `include_bytes!`. Editing `tools/make-icon.mjs` without re-running it and committing the result leaves the old icon in the exe.

What CI enforces on every push to `main` (`.github/workflows/ci.yml`, `windows-latest`) — run these before pushing:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo build --release --locked
cargo test --locked
node tests/render.mjs
```

A running `listen.exe` holds `target/release/listen.exe` open, so `cargo build --release` fails that one link step with `os error 5` while the other four binaries build fine. Stop it with `listen.exe --stop` first, or build the specific binaries you need.

The automated tests cover the frontend (`tests/render.mjs`) and whatever pure byte/string logic exists on the Rust side (`cargo test` — currently just the ICO parsing in `tray.rs`). Everything that touches the OS — SMTC transport, Core Audio volume, media-key injection, sleep suppression, whether the tray icon actually draws — can only be verified by running the real binaries against a live QQ音乐 instance. CI cannot: its runner has no player, no audio device, and is a non-interactive session where SMTC may not even initialize. A green CI does not mean the features work.

Releases are cut by pushing a `v*` tag (`.github/workflows/release.yml`). It checks the tag against `Cargo.toml`'s `version`, so bump that in the same commit as the tag.

Order matters, and getting it wrong is the easiest mistake here: **bump `version`, sync `Cargo.lock` with `cargo update -p cross-next`, commit, then tag.** Tagging before the bump lands produces a tag pointing at a commit whose version does not match, the release job fails its own guard, and no release is created — recovering means moving a published tag. Skipping the lock sync leaves `Cargo.lock` stale, which every `--locked` step rejects; that one fails locally, so it's cheap.

The zip's name deliberately carries no version — README links `/releases/latest/download/` with a fixed filename, so renaming the asset breaks that link.

Every `uses:` must resolve to an action whose `action.yml` declares the Node 24 runtime; the runners no longer ship anything older. Check `runs.using` when pinning or bumping one — major tags are where that changes, and a stale pin fails the whole job rather than warning.

## Binaries

| Binary | Role |
|---|---|
| `cross-next` | The server. Runs on the machine playing music. Tray icon, no console. |
| `remote` | One-shot client. For mouse drivers that can bind "launch program". |
| `listen` | Resident forwarder. Grabs media keys via `RegisterHotKey`, forwards to the server. For drivers offering only preset media-key actions. Has no window or tray icon, so it takes over any previous instance on launch and stops via `--stop`. |
| `probe` | Prints session context, all SMTC sessions, all audio sessions. |
| `keyprobe` | Hooks keyboard/shell/hotkey channels at once to see what a driver actually emits. |

**A diagnostic binary must never be `windows_subsystem = "windows"`.** Its output *is* the program — that is why `probe` and `keyprobe` keep a console while the other three don't. Adding the attribute to one makes a double-click print nothing at all (see the `println!` trap below), and `ui::has_console`/`AttachConsole` only rescues a binary whose output is incidental. Any throwaway probe written to answer a question about a live player belongs in the same camp: console subsystem, and worth also writing its report next to the exe, because a console window that is closed or a session run over a remote desktop takes the scrollback with it. Pair that with a read-back — `accepted=true` from a `Try*Async` is a claim, not evidence, so a probe that only prints the return value can confirm a lie (this is exactly how the seek constraint below got recorded wrong).

`cross-next`, `remote` and `listen` are all `windows_subsystem = "windows"` so they don't flash a console (debug builds keep one). The shared plumbing for that lives in `src/ui.rs`: `AttachConsole(ATTACH_PARENT_PROCESS)` — launched from a shell they print, double-clicked they show a message box — plus the Per-Monitor-v2 DPI declaration, which must happen before any window is created or message boxes render blurry on high-DPI displays.

`ui::has_console` **caches** its answer, and has to: `AttachConsole` fails once it has already succeeded, so a fresh call per site would make every site after the first conclude "no console" and start popping message boxes at command-line users.

The consequence of the server being windowless is easy to forget: **every `println!` in `main.rs` is silent when double-clicked**, and the startup banner is where the access URL comes from. That's why `main.rs` routes all three startup failures through `ui::fail`, why a freshly generated token triggers a one-time message box, and why the tray menu can re-show the URL. Adding a startup path that only prints re-opens the "double-clicked it, nothing happened" hole.

## Architecture

```
browser ──HTTP/JSON──▶ cross-next.exe
                       ├─ main thread: tray icon + message pump
                       ├─ media thread (owns one MTA apartment)
                       │    ├─ SMTC SessionManager  → QQ音乐 transport
                       │    └─ Core Audio sessions  → QQ音乐 per-process volume
                       └─ HTTP accept thread → thread-per-connection (page + JSON API)
```

**The main thread belongs to the tray** (`src/tray.rs`), so `Server::serve()` — which never returns — runs on a named background thread. The pump has to be the main thread, and "quit" is the pump exiting: the HTTP and media threads are torn down with the process rather than signalled.

**One dedicated media thread** (`src/media.rs`) owns the `SessionManager`; HTTP threads talk to it over an `mpsc` channel and wait for a reply. This initializes COM once, keeps WinRT objects off other threads, and reuses a single manager instance — windows-rs [#2061](https://github.com/microsoft/windows-rs/issues/2061) leaks memory if you recreate it in a loop.

**No async runtime.** `src/winrt_block.rs` blocks on `IAsyncOperation` with `SetCompleted` + `Condvar`. windows-future 0.3 only exposes `IntoFuture`; its blocking `Async` trait is private. `block_on` **must** run on an MTA thread — blocking on STA deadlocks for want of a message pump.

**Only dependency is the `windows` crate.** HTTP (`src/http.rs`), the HTTP client (`src/client.rs`), and JSON in/out (`src/jsonlite.rs`) are hand-written. The JSON helpers do targeted extraction from known-shape single-level objects, not general parsing. Don't add axum/tokio/serde/reqwest without a concrete reason.

**Two control modes, auto-detected** (`media::Mode`). `smtc` when QQ音乐 registers an SMTC session: targeted control plus metadata. `mediakey` when it doesn't but Core Audio shows it playing: global `SendInput` media keys, no metadata at all. Real QQ音乐 builds differ on this — one machine registered SMTC with full metadata, another never did and only answered the old `WM_APPCOMMAND` path.

## Hard-won constraints

These came from running the code against real QQ音乐; don't re-derive them.

- **Session isolation.** SMTC sessions are per-Windows-logon-session. Non-interactive sessions see nothing; Win11 throws `0x80070424` from `RequestAsync()`. The server cannot be a service or started over SSH. `probe` checks this first via `ProcessIdToSessionId` vs `WTSGetActiveConsoleSessionId`.
- **Don't gate buttons on `Controls()` capability bits — except seeking, where the bit is the only thing telling the truth.** For transport, QQ音乐 reports them incompletely (`IsPauseEnabled=false` while status is `Opened`), which would grey out the play button forever, so send the command and report the `Try*Async` bool honestly — `false` means the app declined, which is not an error.
  **Seeking is the documented exception, and this file previously had it backwards.** It claimed `TryChangePlaybackPositionAsync` "returns `true` and actually jumps (verified against a live player)". It does not. Re-verified against live QQ音乐 (`QQMusic.exe`, twice, plus a third run): asked to jump from 186.4s to 96.6s it returned `accepted=true` and the position was 189.1s two seconds later — still playing straight through. The whole seek-adjacent family is a no-op that lies: `TryRewindAsync` (×1 and ×4), `TryFastForwardAsync`, `TryChangePlaybackRateAsync` and `TryStopAsync` all return `accepted=true` and never move the position. Meanwhile **every capability bit was accurate**: `IsPlaybackPositionEnabled`, `IsRewindEnabled`, `IsFastForwardEnabled`, `IsStopEnabled`, `IsPlaybackRateEnabled` all `false` and all genuinely dead; `IsPreviousEnabled` and `IsPauseEnabled` `true` and both genuinely work. So for seeking, believe the bit and not the return value — the page disables the slider on `canSeek=false` and says why, and `tests/render.mjs` pins both halves. The old behaviour (ignore the bit, send anyway) is what made dragging the bar silently snap back with no message, which is the bug the user actually reported.
  The lesson generalises: **`accepted=true` is a claim, not evidence.** `seek()` therefore reads the position back (`verify_seek`) and downgrades a false claim to an error, because nothing else can distinguish "jumped" from "lied".
  `TrySkipPreviousAsync` is worth knowing about here too: some players implement it as "replay if past ~3s, else previous track", but QQ音乐 does not — from 61.5s it changed tracks. So there is **no** way to restart the current track over SMTC. The only thing that works is the old `WM_APPCOMMAND` path: media-key Stop followed by PlayPause (~0.7s gap) restarts from the beginning, reproducibly. It was left unimplemented on purpose — it forces playback on (`Paused` becomes `Playing`, so "return to start while staying paused" is impossible), it routes through a `Stopped` state nothing else here handles, the keys are global so another player can take them, and it stalls audibly.
- **`Position` is a snapshot, not a live value.** It is only accurate as of `LastUpdatedTime`; while playing you must add the wall-clock delta since then, or a once-a-second poll returns the same stale number and the bar jumps a notch at a time. `read_timeline` does this **server-side** on purpose — the browser is on another machine, so using its clock would fold in the two machines' clock skew. `TimeSpan::Duration`, `DateTime::UniversalTime` and `FILETIME` all count 100ns ticks, and the latter two share the 1601 epoch, so they subtract directly.
- **`EndTime <= StartTime` means no timeline.** Live streams report it (Edge on a livestream gives `EndTime` of `-0.0`), as does a session that hasn't really started playing. Degrade `position`/`duration` to `null` so the page hides the bar rather than drawing nonsense. Note a session can still claim `IsPlaybackPositionEnabled=true` in this state.
- **The server has `--stop` but deliberately does *not* take over on launch.** `listen` takes over because it has no window and no tray icon — the user can neither see nor close a stray instance, so seizing it is the only decent option. The server has a tray icon, so silently displacing an instance that is actively serving would be unhelpful, and it would break the "change `port` and run a second one" usage that the `AddrInUse` message explicitly offers. What was genuinely missing is the update path: a running exe is locked by the kernel, so it must exit before the file can be overwritten, and clicking the tray's 退出 cannot be scripted. `--stop` fills exactly that gap and nothing more. It reuses the tray window class as its single-instance anchor rather than creating a separate marker window like `listen` does, and sends `WM_CLOSE` rather than killing, so `NIM_DELETE` still runs and no ghost icon is left behind. `FindWindowW` finds one instance without regard to port, so the multi-port case needs repeated calls — acceptable, since that usage is rare.
- **`--version` exists so you can tell what is actually running after an update, and scripts must not read it.** All five binaries report `ui::VERSION` (`env!("CARGO_PKG_VERSION")`); the server and `listen` take `--version`/`-V` ahead of `--stop` and ahead of taking over a previous instance, so asking the version never disturbs a running one. `probe`/`keyprobe` put it in their banner instead, since their output gets pasted into troubleshooting. The tray tooltip carries it too — a double-clicked server never shows the banner, so that is the only place those users can see a version. But **do not capture it from PowerShell**: a windows-subsystem exe invoked with `&` yields `$null` there (bash captures fine, and `Start-Process -RedirectStandardOutput` to a file works), so a version comparison written that way is dead code that silently never fires; worse, if the exe decides it has no console, `ui::report` pops a modal box and a script waits forever. Compare file hashes instead — that is the real question anyway, and it works against older builds that lack the flag.
- **`tools/update.ps1` ships inside the release zip, and needs a UTF-8 BOM.** It locates the install directory via `$PSScriptRoot`, so it only knows what to update when it sits beside the exe — hence the extra `Copy-Item` in `release.yml`'s packaging step. The BOM is load-bearing: PowerShell 5.1 reads a BOM-less `.ps1` as ANSI (GBK here), which mangles every Chinese string until they stop terminating, and the file fails to parse with errors that look like missing braces rather than an encoding problem. Two ordering choices in the script are deliberate: **download and verify before stopping anything**, so a network failure or bad archive leaves the service running rather than stranding the user with nothing; and **copy files over the directory instead of replacing it**, because `config.json` lives beside the exe and deleting the directory would take the token with it, invalidating the browser's and `remote.json`'s copies. It also hashes and unzips through .NET types rather than `Get-FileHash`/`Expand-Archive`: on a machine with PowerShell 7 installed, `PSModulePath` can list PS7's module directory *ahead* of Windows PowerShell's, so 5.1 finds the PS7 Utility module, fails to load it, and `Get-FileHash` raises `CommandNotFoundException` — verified on this machine, where `Expand-Archive` and `Invoke-WebRequest` worked fine at the same time. So testing one cmdlet's availability tells you nothing about another's; .NET types bypass module resolution entirely.
- **Releasing sleep suppression needs a grace period; taking it does not.** "Not playing" has many transient false readings: QQ音乐 briefly reports non-`Playing` while a track loads, network stalls do the same, and the SMTC session itself can vanish for a beat mid-track-change (`find_session` returns `None` and the whole snapshot degrades to defaults). Windows 11 removed the old two-minute grace that used to follow releasing a power request, so the machine is sleep-eligible the instant the request drops — and on a remote-controlled box nobody is touching the keyboard, so the idle timer expired long ago. Together that means a single false reading puts the machine to sleep *then and there*, not 20 seconds of lost suppression. `keepawake::Grace` therefore holds on for `GRACE` (90s) of continuous not-playing before letting go; err long, since the cost of too long is the machine staying up a while after a real pause, and the cost of too short is sleeping mid-playlist. The grace only applies while suppression is already held — otherwise starting the server before opening QQ音乐 would pin the machine awake for no reason. `Grace` is split out from the `SetThreadExecutionState` call precisely so this logic is unit-testable; the syscall is not verifiable in CI.
- **`mediakey` mode has no `PlaybackStatus`, so `playing` must come from Core Audio.** Filling the `mediakey` snapshot's `playing` from `..Default::default()` silently pins it to `false`, which meant sleep suppression never engaged on that path at all while the page displayed "QQ音乐 正在播放". `volume::read` reports `active` (`AudioSessionStateActive` = really rendering audio; paused is `Inactive`) for that purpose, OR-ed across every matching session because the one making sound isn't necessarily first in the enumeration. `probe` prints the state so this is checkable on a real machine.
- **"Replay" is `seek(0)`, and there is no primitive for it.** The whole `Try*` surface on an SMTC session is play/pause/stop/record/fastforward/rewind/skipnext/skipprevious/channelup/down/toggle/autorepeat/playbackrate/shuffle/playbackposition — no restart. Two look close and neither is: `TrySkipPreviousAsync` is implemented by *some* players as "replay if past ~3s, else previous track", but that is each app's own semantics, not something SMTC guarantees, so QQ音乐 cannot be relied on for it; `TryRewindAsync` is continuous scrubbing, not a jump to the start. Don't reach for a prev+next pair either — it is the one option that looks viable in `mediakey` mode (where there is no seek) and is worst exactly there: under shuffle prev is not next's inverse, and on the first track of a playlist a non-wrapping prev makes the following next skip *forward* a song, so a button labelled "replay" silently changes tracks. The failure modes are not comparable — a failed seek does nothing, a failed prev+next leaves the user on a different song. `restart`/`replay` shipped in v0.4.0 as `smtc`-only actions and were removed again in v0.4.1, because the seek they were built on does not work on this QQ音乐 — see the seek entry above. Nothing in `src`, `web` or `tests` refers to them now; what survives is the reasoning, so nobody builds them a second time on a primitive that isn't there.
- **A `hidden` attribute does not hide a `button` here.** The stylesheet gives `button` `display: grid`, which outranks the UA sheet's `[hidden] { display: none }`, so a `hidden` button stays visible. Every hideable control needs its own `#id[hidden] { display: none; }` — `.seek[hidden]` is the one live example (the progress bar); `#restart`/`#mode` also needed it while they existed. The symptom is silent: the attribute is set correctly and the element still draws.
- **Audio sessions are dynamic.** Core Audio lists only sessions that have been active, so QQ音乐 is absent when silent. Re-enumerate on every call; `volume: null` means "unavailable", not zero. Also: the API keys on PID, not name, and one app may hold several sessions — set all matches. `volume.rs` prefers exact process-name matches so a `target` of `qq` doesn't also hit `QQ.exe`.
- **Adapter choice.** Prefer adapters that have a default gateway. WSL2/Hyper-V virtual adapters use `172.x`, pass an RFC1918 check, and are unreachable from other LAN devices. Never bind `0.0.0.0`.
- **Thumbnails are not cached server-side.** SMTC art can lag the track metadata by a beat; caching pins the stale image permanently. The server returns a content hash as `ETag` with `Cache-Control: no-store`, and the page re-checks at 200ms/1.2s/3s after a track change.
- **Honor `Connection: close`.** `client.rs` sends it and reads exactly `Content-Length` bytes. A server that ignores the header while the client waits for EOF stalls until timeout.
- **`CreateIconFromResourceEx` wants one image's resource bits, not a `.ico` file.** Handing it the whole file fails — the directory header isn't part of what it parses. `tray::pick_image` therefore walks `ICONDIR` by hand (6-byte header, then one 16-byte `ICONDIRENTRY` per image) and slices out the entry nearest `SM_CXSMICON`. `dwVer` is `0x00030000`, the icon *resource format* version, unrelated to any Windows version. Byte-offset mistakes here don't error, they just make the icon quietly not appear, so `pick_image` is split out and unit-tested against the real `assets/tray.ico` — both tests were confirmed to fail when the offset field was read from the wrong four bytes.
- **The tray window must not be `HWND_MESSAGE`.** Message-only windows can't take the foreground, and `TrackPopupMenu` requires a foreground window or the menu refuses to dismiss when you click elsewhere. Use an ordinary window without `WS_VISIBLE`. The `PostMessageW(WM_NULL)` after `TrackPopupMenu` is also load-bearing — without it the first click after the menu closes gets swallowed.
- **Handle the `TaskbarCreated` broadcast.** Explorer restarting rebuilds the tray and drops every icon. Ignore the message and the icon is gone for good while the process keeps running — invisible and unkillable from the UI, exactly the trap that made `listen` need `--stop`. The message number only exists at runtime (`RegisterWindowMessageW`), so it can't sit in a `match` pattern; compare before the `match`.
- **`NIM_DELETE` on the way out.** Skip it and the tray keeps a ghost icon until something makes the shell notice the process is gone.
- **There is no way to ask who owns a hotkey.** `RegisterHotKey` failing tells you only that it failed. So `listen` identifies *its own* prior instance instead: a hidden window with a unique class name, found via `FindWindowW` and asked to quit with `WM_CLOSE`. Deliberately not process-name matching plus a kill — `listen.exe` is a generic enough name to hit something unrelated, and `WM_CLOSE` lets the old instance `UnregisterHotKey` on its way out, which a kill does not. Its message loop must call `DispatchMessageW`; `WM_HOTKEY` goes to the thread and is handled inline, but without dispatch the marker window never sees `WM_CLOSE` and takeover silently does nothing.

## Frontend

`web/index.html` is a single file embedded via `include_str!` — rebuild after editing it. Immersive large-cover layout; cover art drives a `--a`/`--b` CSS variable gradient (`pickColors` skips near-greyscale pixels so dark covers don't average to mud). Polls `/api/state` at 1s, updates optimistically on click.

`tests/render.mjs` exercises every `render()` branch: it extracts the `<script>` block and `eval`s it under a stubbed DOM in Node, no dependencies. Add a case there when you add a branch. Three things that will bite you when editing it:

- The stub must grow with the page. Any new DOM API the page touches has to be stubbed or you get a fake failure — `document.documentElement` and element `addEventListener` were both missed on the first attempts, and the since-removed replay-mode toggle needed `title` added. A missing *property* is worse than a missing method: it reads as `undefined` instead of throwing, so a case asserting on it can pass for the wrong reason.
- Not every user-visible bug lives in `render()`. The progress bar froze because `seekDragging` latched in a *pointer handler*, so calling `render()` alone could never catch it. The stub records listeners (`el.fire`, `__fireWindow`) and fakes timers and `performance.now` (`__runTimers`, `__advance`) so a test can drive the real event path. Reach for those before concluding something is untestable here.
- **Verify a new case actually fails without the fix.** Re-introduce the bug, watch it go red, then restore. Both progress-bar cases were confirmed this way; a case that passes either way is worse than none, because it looks like coverage.
- Test code must be appended to the *same* `eval` string. The page is strict-mode, so function declarations inside an `eval` stay scoped to it; a separate `eval` yields `render is not defined`.
- Regexes over `web/index.html` must tolerate CRLF. Git's `autocrlf` checks the file out with `\r\n` on Windows, so a hardcoded `\n` silently matches nothing.

This catches runtime `ReferenceError`s that `node --check` cannot — and those matter here, because an exception partway through `render()` silently kills every update after it (one such bug broke the play icon and the cover art at the same time).

## Conventions

Commit messages are in English. Comments and user-facing strings are in Chinese, matching README. Comments explain *why* — especially where behavior is counterintuitive or was settled by experiment. Keep that; several comments are the only record of a constraint.

`config.json` (server) and `remote.json` (client) live beside the exe, are generated on first run, hold the shared token, and are gitignored. Token comparison is constant-time. Plain HTTP by design — the token blocks fumbles from other devices on the subnet, not sniffing; the port must not be forwarded to the internet.

**One token, three copies, by necessity**: server `config.json`, client `remote.json`, browser localStorage. They sit on different machines, so there is nothing to deduplicate — don't "refactor" them into one. What differs is the reload story, and that asymmetry is deliberate: `listen.exe` hot-reloads `remote.json` because re-registering hotkeys opens a window for another program to steal them, while the server reads `config.json` once at startup because a token change has to invalidate live sessions anyway. `load_or_create` preserves an existing token, so upgrading in place does not invalidate browsers — only unzipping to a *fresh directory* does, because `config.json` is resolved beside the exe. When the page gets a 401 it offers an input box (`parseToken` accepts a bare token, a `"token": "..."` line, a whole JSON blob, or a `?t=` URL) rather than sending the user back to the console.
