import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const read = (relative) => fs.readFileSync(path.join(root, relative), 'utf8');
const packageJson = JSON.parse(read('package.json'));
const packageLock = JSON.parse(read('package-lock.json'));
const tauriConfig = JSON.parse(read('src-tauri/tauri.conf.json'));
const cargoToml = read('src-tauri/Cargo.toml');
const cargoLock = read('src-tauri/Cargo.lock');
const rust = read('src-tauri/src/lib.rs');
const trackerHtml = read('dist/tracker.html');
const readme = read('../README.md');
const changelog = read('CHANGELOG.md');

function scriptBodies(html) {
  return [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)].map((match) => match[1]);
}

test('all release version metadata is synchronized at 2.0.2', () => {
  assert.equal(packageJson.version, '2.0.2');
  assert.equal(packageLock.version, '2.0.2');
  assert.equal(packageLock.packages[''].version, '2.0.2');
  assert.equal(tauriConfig.version, '2.0.2');
  assert.match(cargoToml, /^version = "2\.0\.2"$/m);
  assert.match(cargoLock, /name = "bfr-macro-tracking-utility"\nversion = "2\.0\.2"/);
});

test('Windows process target and product identity are descriptive', () => {
  assert.match(cargoToml, /^name = "bfr-macro-tracking-utility"$/m);
  assert.match(cargoToml, /^name = "bfr_macro_tauri_edition_lib"$/m);
  assert.match(read('src-tauri/src/main.rs'), /bfr_macro_tauri_edition_lib::run\(\)/);
  assert.match(rust, /\.title\(\"BFR Macro & Tracking Utility\"\)/);
  assert.equal(tauriConfig.productName, 'BFR Macro & Tracking Utility');
  assert.equal(tauriConfig.identifier, 'com.bfrmacro.tauri.dev');
  assert.match(rust, /\.tooltip\("BFR Macro & Tracking Utility"\)/);
});

test('stable application identifier and data paths remain compatible', () => {
  assert.equal(tauriConfig.identifier, 'com.bfrmacro.tauri.dev');
  for (const directory of ['Settings', 'Macros', 'Logs', 'webview2-profile']) {
    assert.ok(rust.includes(`app_data.join("${directory}")`), `Missing persisted data directory: ${directory}`);
  }
  assert.match(rust, /run-history-reset-v1\.5\.1-dev\.3\.done/);
});

test('one-time history cleanup guard is retained without creating a new release reset', () => {
  assert.match(rust, /if marker_file\.exists\(\)\s*\{\s*return Ok\(false\);/);
  assert.match(rust, /tracker_sessions_file\)/);
  assert.match(rust, /Non-destructive safeguard retained existing Run History/);
  assert.match(rust, /existing history, settings and saved macros were preserved/);
  assert.doesNotMatch(rust.slice(rust.indexOf('fn reset_run_history_once'), rust.indexOf('fn open_data_folder')), /write_file_atomically\(history_file, b\"\"\)/);
  assert.match(readme, /Do not change this identifier without implementing and testing an explicit data migration/);
});

test('game reward endpoints and canonical Run History fields remain present', () => {
  assert.match(rust, /\/api\/battles\/finish/);
  assert.match(rust, /\/api\/battles\/repeat/);
  for (const field of ['zel', 'karma', 'honor', 'exp', 'run_count', 'batch_id']) {
    assert.ok(rust.includes(field), `Missing reward or batch field: ${field}`);
  }
  for (const heading of ['Completed', 'Time Taken', 'Dungeon', 'Type', 'Runs', 'Zel', 'Karma', 'Honor', 'EXP']) {
    assert.ok(trackerHtml.includes(`<th>${heading}</th>`), `Missing Run History column: ${heading}`);
  }
});

test('tray menu order preserves the established workflow', () => {
  const tray = rust.slice(rust.indexOf('fn build_tray('), rust.indexOf('fn save_settings_on_exit('));
  const menuItems = tray.slice(tray.indexOf('let items:'), tray.indexOf('let menu ='));
  const expected = ['&open,', '&reset,', '&minimize,', 'pin.as_ref(),', '&tracker,'];
  const offsets = expected.map((item) => menuItems.indexOf(item));
  assert.ok(offsets.every((offset) => offset >= 0), 'One or more core tray items are missing');
  assert.deepEqual([...offsets].sort((a, b) => a - b), offsets, 'Core tray item order changed');
});

test('macro actions are tray-controlled and no F8/F9/F10 shortcuts are required', () => {
  for (const item of ['Start Recording', 'Stop Recording', 'Play Macro', 'Stop Playback', 'Loop Continuously']) {
    assert.ok(rust.includes(item), `Missing tray macro state/action: ${item}`);
  }
  for (const event of ['"record" => start_recording(app)', '"play" => start_playback(app.clone())', '"loop" =>']) {
    assert.ok(rust.includes(event), `Missing tray action handler: ${event}`);
  }
  assert.doesNotMatch(`${rust}\n${trackerHtml}`, /\bF(?:8|9|10)\b/i);
});

test('Run History clear remains confirmation-protected and does not target settings/macros', () => {
  assert.match(trackerHtml, /window\.confirm\('Permanently clear all saved Run History records\?/);
  assert.match(trackerHtml, /invoke\('tracker_clear_history'\)/);
  assert.match(rust, /fn tracker_clear_history/);
  assert.match(rust, /battle-rewards\.jsonl/);
  assert.match(rust, /tracker_legacy_file\.with_extension\(\"jsonl\.migrated\"\)/);
  assert.match(rust, /for legacy_copy in \[&self\.tracker_legacy_file, &migrated_legacy_file\]/);
});

test('minimize-to-tray still hides the game window and requests background memory target', () => {
  assert.match(rust, /"minimize" =>\s*\{\s*if let Some\(window\) = window\.as_ref\(\)\s*\{\s*set_webview_memory_target\(app, window, true\);\s*let _ = window\.hide\(\);/);
  assert.match(rust, /observer_version: '2\.0\.0'/);
});

test('Run History normalizes single completions and repeat snapshots without double counting', () => {
  const start = trackerHtml.indexOf('function dungeonLabel(');
  const end = trackerHtml.indexOf('function elapsedTimeText(');
  assert.ok(start >= 0 && end > start, 'Run History normalization functions were not found');
  const normalizers = trackerHtml.slice(start, end);
  const normalize = vm.runInNewContext(`${normalizers}\nnormalize;`, { Map, Number, String, Object });
  const records = [
    {
      record_type: 'battle_reward', path: '/api/battles/finish', batch_id: '', run_count: 1,
      dungeon_id: '100321', dungeon_name: 'Test Dungeon', timestamp_unix_ms: 2000,
      settlement: { zel: 11, karma: 22, honor: 33, exp: 44 }
    },
    {
      record_type: 'battle_reward', path: '/api/battles/repeat', batch_id: 'batch-A', run_count: 2,
      dungeon_id: '100321', timestamp_unix_ms: 3000,
      totals: { zel: 100, karma: 200, honor: 300, exp: 400 }
    },
    {
      record_type: 'battle_reward', path: '/api/battles/repeat', batch_id: 'batch-A', run_count: 5,
      dungeon_id: '100321', timestamp_unix_ms: 5000,
      totals: { zel: 150, karma: 250, honor: 350, exp: 450 }
    },
    {
      record_type: 'battle_reward', path: '/api/battles/repeat', batch_id: 'batch-B', run_count: 3,
      dungeon_id: '100502', timestamp_unix_ms: 4000,
      totals: { zel: 30, karma: 60, honor: 90, exp: 120 }
    }
  ];
  const normalized = normalize(records);
  assert.equal(normalized.length, 3, 'One row per single completion or repeat batch is expected');
  const single = normalized.find((record) => record.path === '/api/battles/finish');
  assert.equal(single.values.runs, 1);
  assert.deepEqual({ ...single.values }, { runs: 1, zel: 11, karma: 22, honor: 33, exp: 44 });
  const batchA = normalized.find((record) => record.batch_id === 'batch-A');
  assert.equal(batchA.values.runs, 5, 'The latest cumulative snapshot should replace the earlier snapshot');
  assert.deepEqual({ ...batchA.values }, { runs: 5, zel: 150, karma: 250, honor: 350, exp: 450 });
});

test('Run History duration display rejects API fallback timestamps', () => {
  const durationSource = trackerHtml.match(/const durationText=ms=>\\{[^\\n]*\\};/);
  const elapsedSource = trackerHtml.match(/function elapsedTimeText\\(record\\)\\{[^\\n]*\\}/);
  assert.ok(durationSource && elapsedSource, 'Run History duration functions were not found');
  const elapsedTimeText = vm.runInNewContext(`${durationSource[0]}\n${elapsedSource[0]}\nelapsedTimeText;`);
  assert.equal(elapsedTimeText({ dungeon_started_at_unix_ms: 1000, timestamp_unix_ms: 19208 }), '18s');
  assert.equal(elapsedTimeText({ dungeon_start_source: 'api_fallback', dungeon_started_at_unix_ms: 1000, timestamp_unix_ms: 19208 }), '—');
  assert.equal(elapsedTimeText({ dungeon_started_at_unix_ms: 0, timestamp_unix_ms: 19208 }), '—');
});

test('frontend and injected JavaScript parse successfully', () => {
  let parsed = 0;
  for (const [file, html] of [['dist/tracker.html', trackerHtml], ['dist/index.html', read('dist/index.html')]]) {
    for (const body of scriptBodies(html)) {
      new vm.Script(body, { filename: `${file}:inline-script-${parsed + 1}` });
      parsed += 1;
    }
  }
  const injectionMatch = rust.match(/const INJECTION_SCRIPT: &str = r#"([\s\S]*?)"#;/);
  assert.ok(injectionMatch, 'Rust game-page injection script was not found');
  new vm.Script(injectionMatch[1], { filename: 'src-tauri/src/lib.rs:INJECTION_SCRIPT' });
  parsed += 1;
  assert.ok(parsed >= 2, 'Expected dashboard and game-page JavaScript to be checked');
});

test('Windows release helper installs dependencies, runs tests, then builds', () => {
  const script = read('BUILD_WINDOWS_RELEASE.ps1');
  assert.match(script, /Invoke-Npm @\('ci'\)/);
  assert.match(script, /Invoke-Npm @\('test'\)/);
  assert.match(script, /Invoke-Npm @\('run', 'build:windows'\)/);
  assert.match(script, /target\\release\\bundle\\nsis/);
  assert.doesNotMatch(script, /1\.4\.5/);
});

test('stable documentation contains no development-only release notes or retired hotkey instructions', () => {
  assert.doesNotMatch(readme, /1\.5\.1-dev\.[123]|BUILD_FIRST|OPTIMIZATION_TEST_NOTES/i);
  assert.doesNotMatch(changelog, /1\.5\.1-dev\.[123]/);
  assert.doesNotMatch(`${readme}\n${changelog}`, /F8|F9|F10/);
  assert.match(readme, /Macro controls are available from the tray; keyboard shortcuts are not required/);
  assert.match(changelog, /^## 2\.0\.0 — Stable release/m);
});

test('Run History preserves old backups, does not rotate, and skips malformed lines', () => {
  assert.match(rust, /fn append_history_record\(path: &Path, record: &Value\)/);
  assert.match(rust, /append_history_record\(&self\.tracker_sessions_file, &record\)/);
  assert.match(rust, /read_jsonl_records\(&backup_file, &mut records, &mut malformed_lines\)/);
  assert.match(rust, /malformed JSONL record/);
  assert.match(rust, /tracker_context_file/);
});

test('repeat batch normalization prefers cumulative settled-run counters', () => {
  assert.match(rust, /fn repeat_cumulative_run_count/);
  assert.match(rust, /valid\(settlement, "settled_runs"\)[\s\S]*valid\(totals, "settled_runs"\)[\s\S]*valid\(totals, "runs"\)/);
  const start = trackerHtml.indexOf('function legacyRunCount(');
  const end = trackerHtml.indexOf('function normalize(', start);
  const source = trackerHtml.slice(start, end);
  const legacyRunCount = vm.runInNewContext(`${source}\nlegacyRunCount;`, { Number });
  assert.equal(legacyRunCount({ path: '/api/battles/repeat', settlement: { runs: 10, settled_runs: 20 }, totals: { runs: 20 } }), 20);
});

test('remote game capability grants only the custom game bridge commands', () => {
  const capability = JSON.parse(read('src-tauri/capabilities/default.json'));
  assert.deepEqual(capability.permissions, ['allow-game-recording-bridge']);
  assert.deepEqual(capability.remote.urls, ['https://rebornbf.com/*']);
});

test('playback sessions are generation-scoped and CDP I/O is time-bounded', () => {
  assert.match(rust, /playback_generation: AtomicU64/);
  assert.match(rust, /playback_session_is_current/);
  assert.match(rust, /TcpStream::connect_timeout/);
  assert.match(rust, /set_read_timeout\(Some\(CDP_IO_TIMEOUT\)\)/);
  assert.match(rust, /set_write_timeout\(Some\(CDP_IO_TIMEOUT\)\)/);
  assert.match(rust, /struct PlaybackSessionGuard/);
});

test('macro loading enforces size, click-count and delay limits', () => {
  assert.match(rust, /MAX_MACRO_FILE_BYTES/);
  assert.match(rust, /MAX_MACRO_CLICKS/);
  assert.match(rust, /MAX_MACRO_DURATION_MS/);
  assert.match(rust, /validate_macro_clicks/);
});

test('macro edge cases restore the previous save and reject overlong recordings', () => {
  assert.match(rust, /No clicks were captured, so the previous saved macro was kept/);
  assert.match(rust, /restore_saved_macro_during_stopping/);
  assert.match(rust, /Recording reached the 24-hour safety limit/);
  assert.match(rust, /Recording reached the \{MAX_MACRO_CLICKS\}-click safety limit/);
});

test('click capture avoids layout reads on large non-interactive ancestors', () => {
  assert.match(rust, /tag === 'html' \|\| tag === 'body'/);
  assert.match(rust, /interactive \? \(node\.innerText \|\| node\.textContent/);
});

test('macro playback follows the original absolute click timeline', () => {
  assert.match(rust, /loop_started\.checked_add\(Duration::from_millis\(click\.elapsed_ms\)\)/);
  assert.match(rust, /checked_add\(LOOP_GAP\)/);
});

test('disk-heavy tracker IPC commands run away from the UI thread', () => {
  for (const name of ['tracker_diagnostic_event', 'tracker_dungeon_context', 'tracker_reward_event', 'tracker_read_data', 'tracker_clear_history']) {
    assert.match(rust, new RegExp(`#\\[tauri::command\\(async\\)\\]\\s*fn ${name}`));
  }
  assert.match(rust, /tracker_dedupe_keys: Mutex<HashSet<String>>/);
  assert.match(rust, /load_history_dedupe_keys/);
});

test('tracker resize saves are debounced and tracker shares the game WebView2 profile', () => {
  const trackerStart = rust.indexOf('fn open_tracker_window(');
  const trackerEnd = rust.indexOf('fn build_tray(', trackerStart);
  const trackerWindow = rust.slice(trackerStart, trackerEnd);
  assert.match(trackerWindow, /data_directory\(profile_dir\)/);
  assert.match(trackerWindow, /resize_save_tx\.send\(\(\)\)/);
  assert.doesNotMatch(trackerWindow, /state\.save\(\)/);
});

test('release helper runs Rust checks before producing the Windows installer', () => {
  const script = read('BUILD_WINDOWS_RELEASE.ps1');
  assert.match(script, /cargo.*test.*--locked/i);
  assert.match(script, /cargo.*clippy.*--locked/i);
});

test('all icons embedded by the Rust tray implementation exist', () => {
  const references = [...rust.matchAll(/include_bytes!\("\.\.\/icons\/(menu\/[^\"]+|icon\.ico)"\)/g)];
  assert.ok(references.length >= 10, 'Expected the app icon and tray menu icons');
  for (const match of references) {
    assert.ok(fs.existsSync(path.join(root, 'src-tauri', 'icons', match[1])), `Missing icon asset: ${match[1]}`);
  }
});

test('recording stop is serialized through a non-startable Stopping state', () => {
  assert.match(rust, /const MODE_STOPPING: u8 = 3/);
  assert.match(rust, /fn begin_stopping_recording\(/);
  assert.match(rust, /fn finish_stopping_recording\(/);
  assert.match(rust, /save_macro_snapshot\(&clicks\)/);
  assert.match(rust, /MODE_STOPPING => return/);
  assert.match(rust, /mode != MODE_STOPPING/);
});

test('reward persistence diagnostics only report records actually appended', () => {
  assert.match(rust, /fn tracker_reward_event\(&self, details: Value\) -> Result<bool, String>/);
  assert.match(rust, /Ok\(true\)/);
  assert.match(rust, /if \(!persisted\) return/);
  assert.match(rust, /event: 'reward_persisted'/);
});

test('settings migration and parsing failures protect the original file', () => {
  assert.match(rust, /settings_save_blocked_reason/);
  assert.match(rust, /Settings were not saved to protect the existing settings file/);
  assert.match(rust, /legacy settings migration failed/);
  assert.match(rust, /Legacy settings migration could not complete; the original file was retained/);
});

test('legacy macro compatibility fixture is explicitly synthetic', () => {
  assert.match(rust, /synthetic_bfrmacro2_fixture_remains_deserializable/);
  assert.match(rust, /Synthetic compatibility fixture only/);
});

test('macro click IPC queue is bounded and stops capture when the bridge stalls', async () => {
  const injectionMatch = rust.match(/const INJECTION_SCRIPT: &str = r#"([\s\S]*?)"#;/);
  assert.ok(injectionMatch, 'Rust game-page injection script was not found');
  const source = injectionMatch[1];
  const start = source.indexOf('// Serialize IPC calls so rapid physical clicks cannot be stored out of order.');
  const end = source.indexOf('        }, true);', start);
  assert.ok(start >= 0 && end > start, 'Physical click queue implementation was not found');
  const queueSource = source.slice(start, end + '        }, true);'.length);

  let pointerHandler;
  const pendingClicks = [];
  const calls = [];
  const context = {
    window: { innerWidth: 1000, innerHeight: 800 },
    document: {
      addEventListener(name, handler) {
        assert.equal(name, 'pointerdown');
        pointerHandler = handler;
      }
    },
    console: { error() {}, info() {} },
    invoke(command, payload) {
      calls.push({ command, payload });
      if (command === 'record_click') {
        return new Promise((resolve) => pendingClicks.push(resolve));
      }
      return Promise.resolve();
    }
  };

  vm.runInNewContext(queueSource, context);
  assert.equal(typeof pointerHandler, 'function', 'Queue must register its physical-click handler');

  const click = { isTrusted: true, button: 0, clientX: 250, clientY: 400 };
  pointerHandler(click);
  await new Promise((resolve) => setImmediate(resolve));
  for (let index = 1; index < 512; index += 1) pointerHandler(click);
  pointerHandler(click);

  const overflowReports = calls.filter((call) => call.command === 'report_recording_error');
  assert.equal(overflowReports.length, 1, 'Overflow must report one recording error');
  assert.match(overflowReports[0].payload.message, /safety limit/);
  assert.ok(calls.length <= 2, 'A stalled bridge must not start an unbounded number of IPC calls');
  assert.equal(pendingClicks.length, 1, 'Only the first serialized click should be in flight');

  pendingClicks[0]();
  await new Promise((resolve) => setImmediate(resolve));
  for (let index = 0; index < 10; index += 1) await Promise.resolve();
  assert.equal(calls.filter((call) => call.command === 'record_click').length, 1,
    'Queued clicks must not be sent after overflow has failed capture');
  pointerHandler(click);
  assert.equal(calls.filter((call) => call.command === 'record_click').length, 1,
    'Further physical clicks must be ignored after overflow');
});
