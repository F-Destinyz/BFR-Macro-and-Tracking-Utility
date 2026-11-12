# BFR Macro & Tracking Utility — Audit patch notes

Scope: record the v2.0.1 post-audit fixes and validation. This work was merged into main by PR #7 (fefaede761dea67b2dbcfaf510ad1b9132ee9778). The follow-up release metadata is prepared separately as v2.0.2.

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
- The frontend emits `reward_persisted` only when the Rust command confirms that a record was appended.

### BUG-06 — legacy migration failures
- Settings, macro, and application-log migration errors are now logged instead of aborting startup.
- Migration continues to copy rather than move/delete legacy files.
- Settings persistence is blocked if a failed settings migration leaves the source uncertain.
- Fundamental app-data directory creation and primary WebView startup errors remain fatal because the application cannot safely operate without those resources.

### PRB-04 — BFRMACRO2 historical compatibility
- **DROPPED / OUT OF SCOPE by product decision.** The user does not intend to use very old BFRMACRO2-style macro files, so obtaining authentic historical fixtures and proving compatibility is no longer a release requirement.
- The synthetic fixture is not evidence of historical compatibility and should not be treated as such.
- Existing loader acceptance of BFRMACRO2 has been left unchanged in this audit patch to avoid an unnecessary breaking change. No promise of historical BFRMACRO2 compatibility is made.

## Preservation checks

The patch does not change the Tauri identifier, app-data paths, WebView2 profile path, macro file format, playback timing, click coordinates, reward calculations, Run History schema, history retention, or tray item order. No cleanup, reset, or deletion is introduced.

## Tests and build verification

The following checks were run successfully on the user's Windows development machine:

- Node release-contract suite: **30 passed**.
- Rust test suite: **14 passed, 0 failed**.
- `cargo fmt --check`: passed.
- `cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`: passed.
- `BUILD_WINDOWS_RELEASE.ps1`: completed successfully and produced the NSIS installer:
  `src-tauri\\target\\release\\bundle\\nsis\\BFR Macro & Tracking Utility_2.0.1_x64-setup.exe` (approximately 3.33 MiB).

The Node suite used `dist/index.html` and `dist/tracker.html` from the previously supplied v2.0.1 audit archive because the new source archive intentionally omitted `dist`. No generated `dist` files are included in this patch.

## PRB-05 — Windows/WebView2 profile sharing

**MANUALLY VERIFIED ON WINDOWS — user-reported pass.**

The user confirmed that the manual application testing passed, including:
- Main application and Run History tracker windows operating together.
- Closing and reopening the tracker window.
- Restarting the application.
- Existing application data and WebView2 profile/session state remaining available.

This is a user-reported manual verification, not an independent reproduction by the audit author. No profile directory was reset or deleted as part of the test.

## Remaining audit scope

The changes above do not claim that every item in the original Claude audit has been fixed. BUG-01, BUG-03, and BUG-07 were handled separately or reviewed and rejected as described in the development history. PRB-04 was explicitly dropped by product decision. PRB-05 passed the user's manual Windows verification. Other potential issues and test gaps from the original audit remain outside this patch unless separately documented and addressed.

## Review note

The `src-tauri/src/lib.rs` diff is large in part because of Rust formatting changes. Review the semantic changes separately from formatting churn before merging. PR #7 has been merged into main. The v2.0.2 version bump and release documentation are prepared on the separate release/v2.0.2 branch for review before publication.
