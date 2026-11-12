# Reliability Fixes — Implementation Notes

## Scope

This source update applies the confirmed and actionable findings from the independent static audit, while retaining the 2.0.0 application identity and existing data formats.

### Implemented

- **Run History startup safety:** the historical dev.3 marker is retained for compatibility, but startup no longer truncates the active history or deletes its `.jsonl.1` backup. Marker-write failure is logged and does not prevent startup.
- **History retention:** reward history no longer uses diagnostic-log rotation. The dashboard reads the active file and the legacy `.jsonl.1` backup. Dungeon-context diagnostics now go to a separate bounded `tracker-context.jsonl` file.
- **History corruption tolerance:** malformed JSONL lines are skipped while valid records continue to load. Each record is serialized into one buffer before append.
- **Migration safety:** legacy history is merged with a `HashSet` rather than repeated full-file scans. The original is retained as a `.migrated` recovery copy. Migration failures are logged without blocking startup. Explicit Clear History removes active history, the rotation backup, and migration recovery copies so old records cannot be re-imported.
- **Repeat accounting:** cumulative counters are preferred over per-chunk `settlement.runs`. This ordering is based on captured game diagnostics where `settlement.runs` stayed at 10 while `settlement.settled_runs` and `totals.runs` advanced to 20 and 30.
- **Playback isolation:** playback workers carry a generation token, so a stopped worker cannot resume as a newer session. A drop guard resets stuck playback state after unexpected worker exit.
- **CDP reliability:** local TCP connect, read, write, HTTP discovery, and WebSocket handshake operations have explicit timeouts.
- **Macro validation:** file size, click count, coordinates, timestamp order, and maximum delay are validated. New recordings are capped consistently. An empty recording restores the previous saved macro in memory.
- **Tracker responsiveness:** disk-heavy tracker commands are dispatched away from the UI thread using Tauri's `#[tauri::command(async)]` mode. Repeat dedupe keys are loaded once and maintained in memory instead of rescanning the entire history for every event.
- **Security:** the remote game-page capability now grants only the custom game bridge permission; broad `core:default` and `notification:default` grants were removed from that remote capability.
- **WebView2 and UI performance:** the tracker uses the configured WebView2 data directory, tracker resize saves use the existing debounce worker, and click capture avoids `innerText` on large non-interactive ancestors.
- **Playback timing:** click deadlines are based on the original macro timeline, avoiding cumulative drift from CDP round-trip latency.
- **Release checks:** the Windows release helper now runs Node tests, Rust unit tests, and Clippy before building the installer.

### Audit items intentionally not changed

- **F-15:** informational permission naming issue; it does not change the effective tracker capability.
- **F-16:** theoretical local-port time-of-check race; current loopback URL validation remains in place. Replacing the CDP port with an in-process WebView2 API is a larger future refactor.
- **F-07 measurement:** async dispatch and dedupe caching address the main UI-thread/full-scan concerns, but runtime timing should still be measured on Windows with a large history file.
- **F-10 measurement:** using the shared WebView2 profile is intended to avoid a second profile/process tree; confirm actual memory/process impact on Windows.

## Compatibility safeguards

- Tauri identifier remains `com.bfrmacro.tauri.dev`.
- Existing `Settings`, `Macros`, `Logs`, and `webview2-profile` locations remain unchanged.
- The existing loader acceptance for older macro files is unchanged, but historical BFRMACRO2 compatibility is not guaranteed and is not a release requirement. Do not remove existing loader acceptance as part of this release.
- The Run History JSONL schema and dashboard columns remain compatible.
- No automatic settings, macro, or history deletion was introduced.
- Release metadata is versioned deliberately for v2.0.2 across package.json, package-lock.json, src-tauri/Cargo.toml, src-tauri/Cargo.lock, and src-tauri/tauri.conf.json.

## Validation performed in this environment

- `npm test`: **26 passed, 0 failed**.
- Node syntax checks for the frontend and injected JavaScript are included in that suite.
- Rust unit tests were added for history preservation, migration, corruption tolerance, repeat counts, macro validation, and playback generation, but were **not executed here** because this environment has no `cargo` or `rustc` installed.
- The Windows installer build and live WebView2/game smoke tests were **not executed here**.

## Required Windows validation before release

1. Back up the existing per-user application data folder.
2. Install over an existing 2.0.0 installation containing settings, a saved macro, `battle-rewards.jsonl`, and (if present) `battle-rewards.jsonl.1`.
3. Confirm startup preserves all existing history, settings, macros, and the WebView2 profile.
4. Confirm Run History shows records from both the active file and the legacy `.1` backup.
5. Confirm explicit Clear History clears active, backup, and migration recovery copies, and that a restart does not re-import them.
6. Test repeat batches with multiple chunks and confirm the displayed run count and rewards are cumulative.
7. Start playback, stop during connection, and immediately start again; confirm only one worker sends clicks.
8. Test a normal macro, continuous looping, an empty recording, and loading a current supported macro file. Historical BFRMACRO2 compatibility is out of release scope.
9. Open and resize Run History while monitoring WebView2 process count and memory.
10. Run `.\BUILD_WINDOWS_RELEASE.ps1` on Windows and confirm the NSIS installer is produced.

## v2.0.2 follow-up validation

The user reported successful Windows validation of the post-audit changes:

- Node release-contract tests: 30 passed.
- Rust unit tests: 14 passed, 0 failed.
- cargo fmt --check: passed.
- cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings: passed.
- BUILD_WINDOWS_RELEASE.ps1 completed and produced the v2.0.1 NSIS installer before the release-version bump.
- Manual testing passed for recording, saving, playback, macro-load protection while recording, main/tracker coexistence, tracker close/reopen, application restart, and preservation of settings, macros, Run History, and WebView2 profile/session data.

Because the application version is now being advanced to v2.0.2, rebuild the installer after this metadata update and perform a final install/launch smoke test before publishing the release. These are user-reported results; they have not been independently reproduced by this documentation update.
