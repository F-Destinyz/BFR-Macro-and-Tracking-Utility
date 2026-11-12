# BFR Macro & Tracking Utility

<p align="center">
  <strong>A Windows desktop companion for Brave Frontier: Reborn</strong><br>
  Macro recording and playback · Dungeon reward tracking · Searchable Run History
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Version-2.0.1-6f42c1?style=for-the-badge" alt="Version 2.0.1">
  <img src="https://img.shields.io/badge/Platform-Windows%2010%20%7C%2011-0078D6?style=for-the-badge" alt="Windows 10 and 11">
  <img src="https://img.shields.io/badge/Status-Stable-2ea44f?style=for-the-badge" alt="Stable release">
</p>

**Current stable release: [v2.0.1](https://github.com/F-Destinyz/BFR-Macro-and-Tracking-Utility/releases/tag/v2.0.1)**

BFR Macro & Tracking Utility is a Windows desktop application for **Brave Frontier: Reborn**. It hosts the game in a Tauri/WebView2 window, provides tray-controlled macro recording and playback, and collects completed dungeon rewards into a searchable Run History.

---

## Contents

- [Highlights](#highlights)
- [Download and install](#download-and-install)
- [Using the application](#using-the-application)
- [Data, privacy, and compatibility](#data-privacy-and-compatibility)
- [Technology](#technology)
- [Build from source](#build-from-source)
- [Testing and release checks](#testing-and-release-checks)
- [Changelog](#changelog)
- [Development assistance and acknowledgement](#development-assistance-and-acknowledgement)
- [Disclaimer](#disclaimer)

## Highlights

### Macro recording and playback

- Record trusted physical left-clicks inside the embedded game window.
- Save click positions as normalized coordinates with their recorded timing.
- Stop recording from the system tray; the recording is saved automatically.
- Start, stop, and loop playback from the system tray.
- Reload the last saved macro from the tray menu.
- Playback sends input through the embedded WebView2 DevTools Protocol and does not move the physical mouse.
- Macro controls are available from the tray; keyboard shortcuts are not required.

### Dungeon reward tracking

- Capture completed reward information from the game's `/api/battles/finish` and `/api/battles/repeat` endpoints.
- Track Zel, Karma, Honor, and EXP separately.
- Count a single battle completion as one run.
- Use cumulative settled-run counters for repeat batches and deduplicate repeat snapshots.
- Start quest timing from the captured Begin Quest or Repeat action click where available. When only API fallback timing is available, the utility avoids presenting a misleading near-zero duration.

### Searchable Run History

The Run History window stores completed dungeon results and supports searching by dungeon or run type. Its table includes:

| Field | Description |
|---|---|
| Completed | Completion date and time |
| Time Taken | Recorded run duration, when available |
| Dungeon | Dungeon or quest name |
| Type | Run type |
| Runs | Number of completed runs represented |
| Zel, Karma, Honor, EXP | Separate reward totals |

Run History can be cleared from the dashboard after a confirmation prompt. Clearing history does not clear settings or saved macros.

### Background operation and system tray

- The game and tracker continue running when the main window is minimized to the system tray.
- The tray menu provides window controls, pinning, Run History, macro controls, folder shortcuts, and Exit.
- Window size, pin state, and loop setting are persisted.
- A single-instance guard prevents multiple application instances from sharing the same files.
- While hidden, the app requests WebView2's low-memory target. This is a runtime hint, not a guarantee of a specific memory reduction.

---

## Download and install

**Get the latest version:** [GitHub Releases](https://github.com/F-Destinyz/BFR-Macro-and-Tracking-Utility/releases/latest)

1. Download the Windows 64-bit setup installer from the latest release.
2. Run the installer.
3. If the Microsoft Edge WebView2 Evergreen Runtime is missing, the installer downloads its bootstrapper. An internet connection is required for that installation.
4. Launch **BFR Macro & Tracking Utility** from the Windows Start Menu or its installed shortcut.
5. Use the application icon in the system tray to control the game window and macro functions.

The installer is configured for the current Windows user. Use the release setup installer rather than copying the application executable by itself.

> **Upgrade note:** The application retains its existing Tauri identifier and per-user data locations. Existing settings, saved macros, logs, and the WebView2 profile are intended to remain available across upgrades.

---

## Using the application

### Macro workflow

1. Open the game through the utility.
2. Use the system tray menu to start recording.
3. Perform the desired in-game clicks.
4. Stop recording from the tray menu. The macro is saved automatically.
5. Use the tray menu to start playback, stop playback, enable looping, or reload the saved macro.

### Run History workflow

1. Complete a dungeon run or repeat batch in the embedded game.
2. The tracker observes the supported battle reward responses.
3. Open **Run History** from the application or tray menu.
4. Search by dungeon or run type and review the recorded rewards.

The collector stores allowlisted reward values and identifiers. It does not write complete response bodies, cookies, or authorization headers to the reward history.

---

## Data, privacy, and compatibility

Application data is stored outside the installation directory. Use the tray menu's folder shortcuts to open the application, Settings, Macros, and Logs locations.

| Location / file | Purpose |
|---|---|
| `Settings\\settings.json` | Window dimensions, pin state, and loop setting |
| `Macros\\recording.bfrmacro` | Last saved macro |
| `Logs\\bfr-macro.log` | Macro and application messages |
| `Logs\\performance.jsonl` | Application and WebView2 process-tree performance samples |
| `Logs\\tracker-discovery.jsonl` | Tracker lifecycle and safe diagnostic events |
| `Logs\\battle-rewards.jsonl` | Persistent sanitized reward records |
| `Logs\\tracker-context.jsonl` | Separate dungeon-context diagnostics |

Run History is retained across upgrades. Startup does not clear existing records, and automatic rotation is disabled for user-facing reward history. Legacy rotation backups remain readable. Legacy migration preserves a recovery copy, and malformed JSONL lines are skipped without suppressing valid neighboring records.

The application deliberately retains its existing Tauri identifier so per-user settings, macros, logs, and WebView2 profile data remain in the same location. Do not change this identifier without implementing and testing an explicit data migration.

---

## Build from source

### Requirements

- Windows 10 or Windows 11.
- Node.js LTS and npm.
- Rust stable with the MSVC target/toolchain.
- Visual Studio Build Tools with the C++ workload and Windows SDK.
- Microsoft Edge WebView2 Evergreen Runtime to run the application.

### Build and verify

Open PowerShell in the `BFR-Utility-Project` project directory and run:

```powershell
npm ci
npm test
cargo test --locked --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run build:windows
```

For the complete Windows release workflow, use the included helper:

```powershell
.\BUILD_WINDOWS_RELEASE.ps1
```

The helper runs the JavaScript contract tests, Rust unit tests, Clippy with warnings treated as errors, and the Windows bundle build. The NSIS setup installer is generated under:

```text
src-tauri\target\release\bundle\nsis\
```

---

## Testing and release checks

The repository includes automated Node.js release-contract tests and Rust unit tests. The Windows release helper runs these checks before creating the installer.

To run the checks individually:

```powershell
npm test
cargo test --locked --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
```

Passing automated tests is useful evidence, but it does not replace manual testing of installation, macro recording and playback, Run History, upgrades, and saved-data preservation on Windows.

---

## Technology

- **Tauri 2** — Windows desktop application shell.
- **Rust** — native application logic and backend.
- **Microsoft Edge WebView2** — embedded game web content.
- **WebView2 DevTools Protocol** — macro playback input.
- **Serde and JSON** — settings, macro, diagnostic, performance, and reward-history data.

---

## Development assistance and acknowledgement

This project was developed by its maintainer with assistance from **OpenAI ChatGPT (GPT-6)**.

AI assistance was used as a development aid for tasks such as:

- Explaining Rust, JavaScript, Tauri, and Windows build concepts.
- Exploring implementation approaches and helping draft code changes.
- Debugging build errors and interpreting compiler and test output.
- Reviewing code for potential reliability, data-preservation, and maintainability issues.
- Preparing regression checks, release documentation, and user-facing instructions.

The maintainer directs the project, reviews and integrates changes, runs the builds and tests, performs Windows application checks, and makes final implementation and release decisions. AI assistance does not replace maintainer review or independent testing.

---

## Changelog

See the project [CHANGELOG](BFR-Utility-Project/CHANGELOG.md) for version history and detailed changes.

For downloads and release notes, see [GitHub Releases](https://github.com/F-Destinyz/BFR-Macro-and-Tracking-Utility/releases).

---

## Disclaimer

This is an independent community utility for Brave Frontier: Reborn. It is not affiliated with, endorsed by, or officially supported by the game's developers or publishers. Use the application and macro features in accordance with the game's rules and your own account's requirements.
