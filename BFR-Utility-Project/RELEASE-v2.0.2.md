# BFR Macro & Tracking Utility v2.0.2

## Reliability and data-preservation maintenance release

This release includes the post-audit fixes for macro state handling, recording lifecycle safety, Run History integrity, settings preservation, reward-persistence diagnostics, and legacy migration error handling.

### Changes
- Prevent late recording-error notifications from interrupting active macro playback.
- Serialize recording, loading, playback, and stop/save transitions so a recording cannot be replaced or restarted before its save completes.
- Preserve JSONL line boundaries when appending after an incomplete final record.
- Protect existing settings from being overwritten when parsing or migration fails.
- Report reward persistence only when a record was actually appended.
- Continue startup after non-fatal legacy migration errors while preserving original legacy files.

### Validation
User-reported Windows validation before the version bump:
- 30 Node tests passed.
- 14 Rust tests passed.
- Formatting check passed.
- Clippy passed with warnings denied.
- Windows NSIS installer build completed.
- Manual recording, saving, playback, tracker-window, restart, and existing-data preservation tests passed.

The v2.0.2 installer must be rebuilt from the release branch and smoke-tested after the version update before publication.

### Upgrade and data preservation
The Tauri identifier remains com.bfrmacro.tauri.dev. Existing application data paths, settings, macros, Run History, and WebView2 profile/session data are intended to remain in place. No automatic data cleanup or history reset is introduced.

Historical BFRMACRO2 compatibility testing is out of scope by product decision; no new compatibility guarantee is made.
