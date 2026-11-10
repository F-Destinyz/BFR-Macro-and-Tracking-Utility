# Changelog

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
