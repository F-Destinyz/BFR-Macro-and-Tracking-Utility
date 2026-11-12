# Changelog

## Unreleased — Reliability and data-preservation fixes

- Remove the startup history wipe; first launch now marks the legacy safeguard without deleting active history or its rotation backup, and marker-write failures no longer prevent startup.
- Stop automatic rotation of user-facing battle history, read legacy `.jsonl.1` backups, isolate dungeon-context diagnostics in `tracker-context.jsonl`, and skip malformed history lines while retaining valid records.
- Make legacy history migration non-destructive and idempotent, preserve the source as a `.migrated` recovery copy, and use a preloaded deduplication-key cache for repeat snapshots.
- Prefer cumulative `settled_runs`/`totals` counters over per-chunk `settlement.runs`, based on captured game response diagnostics.
- Scope playback workers by generation, bound local CDP connection/read/write timeouts, recover playback state after unexpected worker exit, and schedule clicks against the recorded timeline.
- Validate macro file size, click count, coordinates, timestamps, and maximum delay; restore the prior saved macro after an empty recording.
- Reduce remote game-page permissions to the custom game bridge, share the configured WebView2 profile with Run History, debounce tracker-window resize saves, and avoid layout-forcing text reads during click capture.
- Add Rust unit tests and run Rust tests and Clippy in the Windows release helper.

## 2.0.0 — Stable release

- Promote the tested 1.5.1 development line to a stable release after live Windows regression testing.
- Retain the optimized reward observers, reduced diagnostic overhead, and event-driven route tracking.
- Prevent duplicate tracker click-listener initialization and preserve the original Begin Quest/Repeat click timestamps.
- Add confirmed Run History clearing and serialize history reads, writes, and clearing to prevent file-operation races.
- Preserve separate Zel, Karma, Honor, and EXP accounting, single-run counts, and cumulative repeat-batch deduplication.
- Keep macro recording, playback, looping, and loading controlled from the system tray.
- Align release metadata and the Windows executable target name; retain the existing Tauri identifier for per-user data compatibility.
- Add automated release-contract and JavaScript syntax checks to the Windows release workflow.

## 1.5.0 — Version update

- Bump application version metadata to 1.5.0; no functional changes from v1.4.21.

## 1.4.21 — Tray menu and Run History polish

- Place Run History immediately below Pin Window in the system tray menu and give it a dedicated gold ledger/clock icon.
- Move Reset Window Size above Minimize to Tray.

## 1.4.20 — Repeat-batch timing correction

- Preserve the original Begin Quest click timestamp when the first repeat API response identifies a repeat batch.
- Use the API response as the timer start only when no valid matching click timestamp was captured.

## 1.4.18 — Run History-only dashboard

- Make the searchable Run History table the dashboard's main view and remove the Live Run presentation.
- Retain reward capture, history persistence, repeat-batch normalization/deduplication, pinning, refresh, and window-size persistence.

## 1.4.5 — Honor and quest timer correction

- Add Honor as a separate reward value alongside Zel, Karma, and EXP.
- Improve quest and repeat-batch elapsed-time handling.

## 1.4.4 — Zel coin icon

- Replace the letter-based Zel icon with a coin-only icon.

## 1.4.1 — Reward accounting and deduplication

- Normalize single completions to one run and use cumulative settled-run counts for repeat batches.
- Deduplicate repeat snapshots by batch ID and cumulative run count.
