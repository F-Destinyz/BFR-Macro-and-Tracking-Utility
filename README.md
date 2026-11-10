# BFR Macro & Tracking Utility

**Stable release: 2.0.0**

A Windows desktop utility for Brave Frontier: Reborn. It embeds the game in a Tauri/WebView2 window, provides tray-controlled macro recording and playback, and records completed dungeon rewards in a searchable Run History window.

## Features

### Tray-controlled macro utility

- Record trusted physical left-clicks inside the game with normalized coordinates and elapsed timing.
- Stop recording from the system tray; the recording is saved automatically.
- Start playback, stop playback, and toggle continuous looping from the system tray.
- Reload the last saved macro from the system tray.
- Macro controls are operated through the tray menu; keyboard shortcuts are not required.
- Playback dispatches input through the embedded WebView2 DevTools Protocol without moving the physical mouse.

### Dungeon Run History

- Capture completed reward responses from `/api/battles/finish` and `/api/battles/repeat`.
- Start quest timing from the actual Begin Quest or Repeat action click when it is captured; API fallback timing is shown as unavailable rather than as a misleading near-zero duration.
- Treat a single `/api/battles/finish` completion as exactly one run.
- Use cumulative settled-run counts for repeat batches and deduplicate snapshots by batch ID and cumulative run count.
- Keep Zel, Karma, Honor, and EXP as separate reward values.
- Search saved history by dungeon or run type. The table shows Completed, Time Taken, Dungeon, Type, Runs, Zel, Karma, Honor, and EXP.
- Clear Run History from the dashboard with a confirmation prompt. This does not clear settings or saved macros.

### Background operation and saved data

- The game and tracker continue running when the main window is minimized to the system tray.
- WebView2's low-memory target is requested while hidden; this is a runtime hint and does not guarantee a specific memory reduction.
- Settings, saved macros, logs, and the WebView2 profile are stored outside the installation directory.
- Window size, pin state, and loop setting are persisted.
- A single-instance guard prevents a second application instance from sharing the same files.

## System tray

The tray menu provides the game window, window sizing and minimization, pinning, Run History, macro recording/playback/loop controls, macro loading, application/data-folder shortcuts, and Exit. Recording and playback menu labels change with the current macro state.

## Installation

1. Run the **BFR Macro & Tracking Utility** Windows setup installer.
2. If the Microsoft WebView2 Evergreen Runtime is missing, the installer downloads its bootstrapper; an internet connection is required.
3. Launch **BFR Macro & Tracking Utility** from the Windows Start Menu or its installed shortcut.
4. Use the application icon in the system tray to control the game window and macro functions.

The installer is configured for the current Windows user. Use the installer produced by the release build rather than copying the executable alone.

## Build from source

### Requirements

- Windows 10 or Windows 11.
- Node.js LTS and npm.
- Rust stable with the MSVC target/toolchain.
- Visual Studio Build Tools with the C++ workload and Windows SDK.
- WebView2 Evergreen Runtime for running the application.

### Build steps

Open PowerShell in the project directory and run:

```powershell
npm ci
npm test
npm run build:windows
```

Or run the release helper script:

```powershell
.\BUILD_WINDOWS_RELEASE.ps1
```

The release checks use Node.js built-in test tools and do not require a separate test framework. The NSIS setup installer is generated under `src-tauri\target\release\bundle\nsis\`.

## Application data and logs

Use the tray menu's folder shortcuts to open the application, Settings, Macros, and Logs locations.

| File | Purpose |
|---|---|
| `Settings\settings.json` | Window dimensions, pin state, and loop setting |
| `Macros\recording.bfrmacro` | Last saved macro |
| `Logs\bfr-macro.log` | Macro and application messages |
| `Logs\performance.jsonl` | Application and WebView2 process-tree performance samples |
| `Logs\tracker-discovery.jsonl` | Tracker lifecycle and safe diagnostic events |
| `Logs\battle-rewards.jsonl` | Persistent sanitized reward records |

The reward collector stores allowlisted reward values and identifiers. It does not write complete response bodies, cookies, or authorization headers to the reward history.

**Upgrade compatibility:** the Tauri application identifier is intentionally retained from the development build so existing settings, macros, logs, and WebView2 profile data remain in the same per-user data location. Do not change it without adding and testing an explicit data migration.

## Technology

- Tauri 2 desktop shell and Rust backend.
- Microsoft Edge WebView2 runtime.
- WebView2 DevTools Protocol for macro playback.
- Serde/JSON for settings, macro, diagnostic, performance, and reward-history data.

This is a personal utility for Brave Frontier: Reborn and is not affiliated with the game's developers.
