# BFR Macro & Tracking Utility — Audit patch notes

Scope: follow-up review of the supplied v2.0.1 audit source. This patch is intended for the existing `audit/v2.0.1-code-review` branch only. It does not change the stable `main` branch.

## Changes in this patch

### BUG-02 — recording stop/save race
- Added `MODE_STOPPING` as a temporary state between Recording and Idle.
- Stopping and recording-error recovery now take an immutable click snapshot while holding the same timer/click locks used by recording startup.
- The completed snapshot is saved before the mode returns to Idle. Recording, loading, and playback cannot start during this interval.
- Empty recordings restore the previous saved macro into memory while still in Stopping.
- Tray controls display `Saving Recording...` and remain disabled during the transition.
- Added Rust regression tests for restart/load/playback rejection during Stopping and for error events not ending playback/idle state.

### BUG-04 — settings preservation
- Existing settings that cannot be read or parsed no longer silently become writable defaults.
- Settings writes are blocked while the existing file is in an uncertain state, preserving its original bytes for manual recovery.
- If legacy settings migration fails, the original legacy file is still read when possible, but writes remain blocked until migration is resolved.
- Added a regression test proving a blocked save leaves malformed original bytes unchanged.

### BUG-05 — reward persistence diagnostics
- The Rust reward command now returns `true` only after a record is appended and `false` for ignored or duplicate events.
- The frontend emits `reward_persisted` only when the command returns `true`.

### BUG-06 — legacy migration failures
- Settings, macro, and application-log migration errors are now logged instead of aborting startup.
- Migration continues to copy rather than move/delete legacy files.
- Settings persistence is blocked if a failed settings migration leaves the source uncertain.
- Fundamental app-data directory creation and primary WebView startup errors remain fatal because the application cannot safely operate without those resources.

### PRB-04 — BFRMACRO2 test coverage
- Added a synthetic BFRMACRO2 JSON fixture test for the currently supported schema.
- This is not an authentic file produced by an older release and therefore does not prove historical compatibility. A real BFRMACRO2 file is still needed for that.

## Preservation checks

The patch does not change the Tauri identifier, app-data paths, WebView2 profile path, macro file format, playback timing, click coordinates, reward calculations, Run History schema, history retention, or tray item order. No cleanup, reset, or deletion is introduced.

## Tests and limitations

- Node release-contract suite: 30 tests expected after the four new contract checks; run result recorded in the accompanying delivery message.
- Rust tests and Clippy were not executable in the patch-generation environment because `cargo`, `rustc`, and `rustfmt` are not installed. They must be run on the user's Windows development machine before committing.
- The Node suite used `dist/index.html` and `dist/tracker.html` from the previously supplied v2.0.1 audit archive because the new source archive intentionally omitted `dist`. No generated `dist` files are included in this patch.
- PRB-05 requires manual Windows/WebView2 testing. See the checklist below.

## PRB-05 — Windows manual verification checklist

1. Launch the app normally; confirm the main game window opens without a WebView2 profile error.
2. Open Run History; confirm the tracker window opens while the main window remains usable.
3. Navigate in both windows and confirm each retains its expected page/session state.
4. Close only the tracker window, reopen it, and verify its session/local storage remains available.
5. Close and restart the application; verify the existing game login/session and tracker state are still available.
6. Confirm the app continues using the existing `%APPDATA%` application data location and `webview2-profile` directory. Do not delete, rename, or reset that directory during the test.
7. Check diagnostics for WebView2 profile lock, environment, or initialization errors.
8. Repeat with the tracker opened before navigating the main window, then reverse the order.

Record the exact result and any error text. Do not treat successful creation of both windows alone as proof that all shared profile data is preserved.
