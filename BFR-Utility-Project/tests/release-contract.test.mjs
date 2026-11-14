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
const buildRs = read('src-tauri/build.rs');
const rust = read('src-tauri/src/lib.rs');
const trackerHtml = read('dist/tracker.html');
const trackerCapability = JSON.parse(read('src-tauri/capabilities/tracker.json'));
const readme = read('../README.md');
const changelog = read('CHANGELOG.md');
const applicationWorkflow = read('../.github/workflows/application-checks.yml');
const securityWorkflow = read('../.github/workflows/security-audit.yml');

function scriptBodies(html) {
  return [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)].map((match) => match[1]);
}

test('all release version metadata is synchronized at 2.0.3', () => {
  assert.equal(packageJson.version, '2.0.3');
  assert.equal(packageLock.version, '2.0.3');
  assert.equal(packageLock.packages[''].version, '2.0.3');
  assert.equal(tauriConfig.version, '2.0.3');
  assert.match(cargoToml, /^version = "2\.0\.3"$/m);
  assert.match(cargoLock, /name = "bfr-macro-tracking-utility"\r?\nversion = "2\.0.3"/);
});


test('build runtimes and WebView dependencies use the current compatible releases', () => {
  assert.match(cargoToml, /wry = "=0\.57\.2"/);
  assert.match(cargoLock, /name = "wry"\r?\nversion = "0\.57\.2"/);
  assert.match(applicationWorkflow, /actions\/checkout@v7/);
  assert.match(applicationWorkflow, /actions\/setup-node@v7/);
  assert.match(applicationWorkflow, /node-version: 26/);
  assert.match(securityWorkflow, /actions\/checkout@v7/);
  assert.match(securityWorkflow, /actions\/setup-node@v7/);
  assert.match(securityWorkflow, /node-version: 26/);
  assert.match(readme, /Node\.js 26 LTS/);
});

test('CI builds a Windows installer artifact without publishing GitHub releases', () => {
  assert.match(applicationWorkflow, /actions\/upload-artifact@v7/);
  assert.match(applicationWorkflow, /name: windows-nsis-installer/);
  assert.match(applicationWorkflow, /retention-days: 7/);
  assert.doesNotMatch(applicationWorkflow, /publish-release|gh release create|actions\/download-artifact/);
  assert.match(securityWorkflow, /workflow_call:/);
  assert.doesNotMatch(securityWorkflow, /^  push:/m);
});

test('Windows CI runs a non-gating synthetic Run History performance baseline', () => {
  assert.match(rust, /fn history_performance_baseline_large_synthetic_history/);
  assert.match(rust, /manual performance baseline; CI runs this explicitly on Windows/);
  assert.match(applicationWorkflow, /Run synthetic Run History performance baseline/);
  assert.match(applicationWorkflow, /history_performance_baseline_large_synthetic_history -- --ignored --nocapture/);
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
  assert.match(rust, /if path_entry_exists\(&marker_file\)\.map_err\(/);
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
    assert.ok(trackerHtml.includes(`<th scope="col">${heading}</th>`), `Missing Run History column: ${heading}`);
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

test('Run History reads and clear operations cannot race each other', () => {
  assert.match(trackerHtml, /historyMutationInProgress=false/);
  assert.match(trackerHtml, /if\(loadingPage\|\|historyMutationInProgress\)return/);
  assert.match(trackerHtml, /clearHistoryButton\.disabled=true;refreshButton\.disabled=true/);
  assert.match(trackerHtml, /loadOlderButton\.disabled=true/);
  assert.match(trackerHtml, /finally\{loadingPage=false;loadOlderButton\.disabled=!hasMore;clearHistoryButton\.disabled=false;refreshButton\.disabled=false;\}/);
  assert.match(trackerHtml, /historyMutationInProgress=true/);
  assert.match(trackerHtml, /historyMutationInProgress=false/);
  assert.match(trackerHtml, /if\(clearError\)\{\s*await load\(false\)/);
  assert.match(trackerHtml, /status\.textContent=clearError/);
});

test('Run History clear remains confirmation-protected and does not target settings/macros', () => {
  assert.match(trackerHtml, /window\.confirm\('Permanently clear all saved Run History records\?/);
  assert.match(trackerHtml, /invoke\('tracker_clear_history'\)/);
  assert.match(rust, /fn tracker_clear_history/);
  assert.match(rust, /battle-rewards\.jsonl/);
  assert.match(rust, /tracker_legacy_file\.with_extension\(\"jsonl\.migrated\"\)/);
  assert.match(rust, /fn quarantine_history_files\(/);
  assert.match(rust, /fn restore_quarantined_history_files\(/);
  assert.match(rust, /quarantine_history_files\(&\[/);
  assert.match(rust, /history_clear_stage_error/);
  assert.match(rust, /original files were restored/);
  assert.match(rust, /fn delete_quarantined_history_files/);
  assert.match(rust, /could not be permanently deleted/);
  assert.match(rust, /history_clear_reports_quarantined_files_that_cannot_be_deleted/);
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
  const durationSource = trackerHtml.match(/const durationText=ms=>\{[\s\S]*?\};/);
  const elapsedSource = trackerHtml.match(/function elapsedTimeText\(record\)\{[\s\S]*?\}/);
  assert.ok(durationSource && elapsedSource, 'Run History duration functions were not found');
  const elapsedTimeText = vm.runInNewContext(durationSource[0] + '\n' + elapsedSource[0] + '\nelapsedTimeText;');
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

test('user-data path inspection errors are not treated as missing files', () => {
  assert.match(rust, /fn path_entry_exists\(path: &Path\) -> std::io::Result<bool>/);
  assert.match(rust, /fs::symlink_metadata\(path\)/);
  assert.match(rust, /fn resolve_settings_source\(/);
  assert.match(rust, /path_entry_exists\(destination\)/);
  assert.match(rust, /path_entry_exists\(legacy\)/);
  assert.match(rust, /path_entry_exists\(&recovery_copy\)/);
  assert.match(rust, /path_entry_exists\(&macro_state\.file\)/);
  assert.match(rust, /path_entry_exists\(&self\.file\)/);
  assert.match(rust, /path_entry_exists\(&marker_file\)/);
  assert.match(rust, /fn path_entry_exists_distinguishes_missing_present_and_inspection_errors/);
  assert.match(rust, /fn settings_source_prefers_new_location_and_falls_back_only_when_missing/);
  assert.match(rust, /fn legacy_migrations_propagate_path_inspection_errors/);
  assert.doesNotMatch(rust, /settings_file\.exists\(\)|settings_source\.exists\(\)|macro_state\.file\.exists\(\)/);
});

test('legacy settings, macro and log migration uses an atomic staged copy', () => {
  const start = rust.indexOf('fn copy_file_atomically(');
  const end = rust.indexOf('fn migrate_tracker_history(', start);
  assert.ok(start >= 0 && end > start, 'Atomic migration copy helper was not found');
  const migration = rust.slice(start, end);
  assert.match(migration, /create_new\(true\)/);
  assert.match(migration, /std::io::copy\(&mut source_file, &mut temporary_file\)/);
  assert.match(migration, /temporary_file\.sync_all\(\)/);
  assert.match(migration, /replace_file\(&temporary_path, destination\)/);
  assert.match(migration, /copy_file_atomically\(legacy, destination\)/);
  assert.match(rust, /legacy_file_migration_copies_atomically_and_preserves_original/);
  assert.match(rust, /atomic_migration_copy_failure_removes_temporary_file/);
  assert.match(rust, /atomic_migration_replace_failure_preserves_destination_and_cleans_temporary_file/);
});

test('Run History page loading propagates second-pass read errors', () => {
  const start = rust.indexOf('fn read_jsonl_page(');
  const end = rust.indexOf('fn load_history_dedupe_keys(', start);
  assert.ok(start >= 0 && end > start, 'Run History page reader was not found');
  const pageReader = rust.slice(start, end);
  assert.doesNotMatch(pageReader, /let _ = scan_jsonl_records/);
  assert.match(pageReader, /scan_jsonl_records\(path, \|record\|[\s\S]*?\}\)\?;/);
});

test('Run History startup loads deduplication keys as a stream', () => {
  const start = rust.indexOf('fn load_history_dedupe_keys(');
  const end = rust.indexOf('fn repeat_cumulative_run_count(', start);
  assert.ok(start >= 0 && end > start, 'Run History deduplication loader was not found');
  const loader = rust.slice(start, end);
  assert.match(loader, /scan_jsonl_records\(path,/);
  assert.doesNotMatch(loader, /let mut records = Vec::new\(\)|read_jsonl_records/);
  assert.match(rust, /fn history_dedupe_keys_stream_from_backup_and_active_files\(/);
  assert.match(rust, /fn ensure_history_dedupe_keys_loaded\(/);
  assert.match(rust, /history_dedupe_cache_retries_after_initial_read_failure/);
  assert.match(rust, /tracker_dedupe_keys_ready: AtomicBool/);
});

test('Run History scanner skips only missing files and propagates other open errors', () => {
  const start = rust.indexOf('fn scan_jsonl_records<');
  const end = rust.indexOf('#[cfg(test)]', start);
  assert.ok(start >= 0 && end > start, 'Run History scanner was not found');
  const scanner = rust.slice(start, end);
  assert.doesNotMatch(scanner, /path\.exists\(\)/);
  assert.match(scanner, /ErrorKind::NotFound/);
  assert.match(rust, /history_scanner_skips_missing_files_and_propagates_read_errors/);
});

test('Run History startup warning accurately describes fail-closed reward handling', () => {
  assert.match(rust, /deduplicated reward events will be rejected until history can be read/);
  assert.doesNotMatch(rust, /new events will still be recorded/);
});

test('Run History reloads deduplication keys before appending after startup read failures', () => {
  const start = rust.indexOf('fn tracker_reward_event(');
  const end = rust.indexOf('fn tracker_read_data_page(', start);
  assert.ok(start >= 0 && end > start, 'Run History reward handler was not found');
  const handler = rust.slice(start, end);
  const reload = handler.indexOf('ensure_history_dedupe_keys_loaded(');
  const append = handler.indexOf('append_history_record(');
  assert.ok(reload >= 0 && append > reload,
    'Reward deduplication must recover the on-disk key cache before appending records');
  assert.match(handler, /if let Some\(key\) = dedupe_key\.as_deref\(\)/);
});

test('Run History preserves old backups, does not rotate, and skips malformed lines', () => {
  assert.match(rust, /fn append_history_record\(path: &Path, record: &Value\)/);
  assert.match(rust, /append_history_record\(&self\.tracker_sessions_file, &record\)/);
  assert.match(rust, /fn read_jsonl_page\(/);
  assert.match(rust, /MAX_HISTORY_PAGE_SIZE: usize = 200/);
  assert.match(rust, /scan_jsonl_records\(path, \|record\| records\.push\(record\)\)/);
  assert.match(rust, /malformed(?: or oversized)? JSONL record/);
  assert.match(rust, /tracker_read_data_page/);
  assert.doesNotMatch(rust, /fn tracker_read_data\(/);
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

test('local WebView Content Security Policy restricts resources and preserves Tauri IPC', () => {
  const csp = tauriConfig.app.security.csp;
  assert.ok(csp && typeof csp === 'object', 'A production CSP must be configured');
  assert.equal(csp['default-src'], "'self'");
  assert.equal(csp['script-src'], "'self'");
  assert.equal(csp['connect-src'], "'self' ipc: http://ipc.localhost");
  assert.equal(csp['object-src'], "'none'");
  assert.equal(csp['base-uri'], "'self'");
  assert.doesNotMatch(csp['script-src'], /unsafe-inline|unsafe-eval/);
  assert.match(csp['style-src'], /'unsafe-inline'/);
  assert.match(csp['img-src'], /asset:.*data:/);
  assert.match(csp['font-src'], /'self'/);
  assert.doesNotMatch(trackerHtml, /<(?:script|img|iframe|audio|video)\b[^>]*\b(?:src|href)=["']https?:/i);
  assert.doesNotMatch(trackerHtml, /<(?:script|img|iframe|audio|video)\b[^>]*\b(?:src|href)=["']\/\//i);
});

test('remote game capability grants only the custom game bridge commands', () => {
  const capability = JSON.parse(read('src-tauri/capabilities/default.json'));
  assert.deepEqual(capability.permissions, ['allow-game-recording-bridge']);
  assert.deepEqual(capability.remote.urls, ['https://rebornbf.com/*']);
});

test('tracker window grants only its custom IPC permissions and active diagnostics are bounded', () => {
  assert.deepEqual(trackerCapability.permissions, ['allow-tracker-dashboard-controls']);
  const allowlist = rust.match(/let safe_event = match event \{([\s\S]*?)\n\s*_ => return Err/);
  assert.ok(allowlist, 'Missing tracker diagnostic event allowlist');
  const events = [...allowlist[1].matchAll(/"([a-z_]+)"/g)].map((match) => match[1]).sort();
  assert.deepEqual(events, [
    'begin_quest_click_seen',
    'dungeon_context_seen',
    'observer_error',
    'observer_started',
    'observer_status',
    'repeat_action_click_seen',
    'reward_persist_failed',
    'reward_persisted'
  ]);
  assert.match(rust, /MAX_TRACKER_DIAGNOSTICS_PER_WINDOW: u32 = 120/);
  assert.match(rust, /tracker_diagnostic_rate_limit: Mutex<TrackerDiagnosticRateLimit>/);
  assert.match(rust, /fn sanitize_recording_error_message\(message: &str\)/);
  assert.match(rust, /\.take\(MAX_RECORDING_ERROR_MESSAGE_CHARS\)/);
  assert.match(rust, /MAX_RECORDING_ERROR_MESSAGE_CHARS: usize = 500/);
  assert.doesNotMatch(rust, /"network_activity" =>| "websocket_activity" =>| "dom_activity" =>| "navigation_activity" =>/);
});

test('Tauri app command manifest matches every registered IPC command and dashboard permission', () => {
  const handler = rust.match(/invoke_handler\(tauri::generate_handler!\[([\s\S]*?)\]\)/)?.[1];
  const manifest = buildRs.match(/commands\(&\[([\s\S]*?)\]\)/)?.[1];
  assert.ok(handler, 'Missing Tauri invoke handler command list');
  assert.ok(manifest, 'Missing Tauri AppManifest command list');
  const registered = [...handler.matchAll(/^\s*([a-z][a-z0-9_]*)\s*,?\s*$/gm)].map((match) => match[1]).sort();
  const declared = [...manifest.matchAll(/"([a-z][a-z0-9_]*)"/g)].map((match) => match[1]).sort();
  assert.deepEqual(declared, registered, 'Every registered command must be included in AppManifest');
  const dashboard = read('src-tauri/permissions/tracker-dashboard.toml');
  assert.match(dashboard, /commands\.allow = \["tracker_read_data_page", "tracker_clear_history", "tracker_get_pinned", "tracker_set_pinned"\]/);
  assert.match(dashboard, /commands\.deny = \["tracker_read_data_page"\]/);
  assert.doesNotMatch(dashboard, /\btracker_read_data\b/);
});

test('playback sessions are generation-scoped and CDP I/O is time-bounded', () => {
  assert.match(rust, /playback_generation: AtomicU64/);
  assert.match(rust, /playback_session_is_current/);
  assert.match(rust, /TcpStream::connect_timeout/);
  assert.match(rust, /set_read_timeout\(Some\(CDP_IO_TIMEOUT\)\)/);
  assert.match(rust, /set_write_timeout\(Some\(CDP_IO_TIMEOUT\)\)/);
  assert.match(rust, /struct PlaybackSessionGuard/);
});

test('playback rechecks its session after viewport lookup before dispatching a click', () => {
  const start = rust.indexOf('fn start_playback(');
  const end = rust.indexOf('\nfn ', start + 1);
  assert.ok(start >= 0 && end > start, 'Playback worker implementation was not found');
  const worker = rust.slice(start, end);
  const viewportLookup = worker.indexOf('viewport_size(');
  const sessionRecheck = worker.indexOf('if !playback_session_is_current(', viewportLookup);
  const clickDispatch = worker.indexOf('dispatch_click(', sessionRecheck);
  assert.ok(viewportLookup >= 0 && sessionRecheck > viewportLookup && clickDispatch > sessionRecheck,
    'The playback generation must be rechecked after viewport lookup and before click dispatch');
});

test('application shutdown stops macro activity and saves settings', () => {
  const start = rust.lastIndexOf('window.on_window_event');
  const end = rust.indexOf('tauri::WindowEvent::Resized', start);
  assert.ok(start >= 0 && end > start, 'Main window event handler was not found');
  const closeHandler = rust.slice(start, end);
  assert.match(closeHandler, /CloseRequested/);
  assert.match(closeHandler, /stop_recording\(&app_handle\)/);
  assert.match(closeHandler, /stop_playback\(&app_handle\)/);
  assert.match(closeHandler, /save_settings_on_exit\(&app_handle\)/);
  assert.match(closeHandler, /app_handle\.exit\(0\)/);
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
  assert.match(rust, /playback_click_deadline\(loop_started, click\.elapsed_ms\)/);
  assert.match(rust, /fn playback_click_deadline\(loop_started: Instant, elapsed_ms: u64\)[\s\S]*?loop_started\.checked_add\(Duration::from_millis\(elapsed_ms\)\)/);
  assert.match(rust, /checked_add\(LOOP_GAP\)/);
});

test('disk-heavy tracker IPC commands run away from the UI thread', () => {
  for (const name of ['tracker_diagnostic_event', 'tracker_dungeon_context', 'tracker_reward_event', 'tracker_read_data_page', 'tracker_clear_history']) {
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

test('macro file loading and restore share validated BFRMACRO2/BFRMACRO3 parsing', () => {
  assert.match(rust, /fn parse_macro_file\(bytes: &\[u8\]\) -> Result<MacroFile, String>/);
  assert.match(rust, /data\.format != "BFRMACRO2" && data\.format != "BFRMACRO3"/);
  assert.match(rust, /validate_macro_clicks\(&data\.clicks\)/);
  const parserCalls = rust.match(/let data = parse_macro_file\(&bytes\)\?/g) || [];
  assert.equal(parserCalls.length, 2, 'Load and restore must use the shared validator');
  assert.match(rust, /macro_file_parser_accepts_both_supported_formats/);
  assert.match(rust, /macro_file_parser_rejects_unsupported_formats_and_invalid_clicks/);
  assert.match(rust, /macro_file_serialization_round_trips_as_current_format/);
});

test('unreadable saved macros cannot be overwritten by later recordings', () => {
  assert.match(rust, /macro_save_blocked_reason: Mutex<Option<String>>/);
  assert.match(rust, /fn save_macro_snapshot_to_file\(/);
  assert.match(rust, /fn block_macro_saves\(&self, reason: String\)/);
  assert.match(rust, /automatic replacement is disabled until it can be loaded or removed/);
  assert.match(rust, /path inspection failed: \{error\}/);
  assert.match(rust, /fn blocked_macro_save_preserves_existing_file\(/);
  assert.match(rust, /fn manually_removed_saved_macro_clears_the_save_block\(/);
  assert.match(rust, /fn existing_saved_macro_keeps_the_save_block_until_loaded\(/);
  assert.match(rust, /fn macro_save_block_remains_when_path_cannot_be_inspected\(/);
  assert.match(rust, /fn empty_macro_snapshot_remains_a_no_op_even_when_saving_is_blocked\(/);
  const loadStart = rust.indexOf('fn load_macro(&self)');
  const loadEnd = rust.indexOf('fn log(&self, message: &str)', loadStart);
  assert.ok(loadStart >= 0 && loadEnd > loadStart, 'Saved macro load method was not found');
  assert.match(rust.slice(loadStart, loadEnd), /\*save_blocked_reason = None/);
  const restoreStart = rust.indexOf('fn restore_saved_macro_during_stopping(&self)');
  const restoreEnd = rust.indexOf('fn load_macro(&self)', restoreStart);
  assert.ok(restoreStart >= 0 && restoreEnd > restoreStart, 'Saved macro restore method was not found');
  assert.match(rust.slice(restoreStart, restoreEnd), /\*save_blocked_reason = None/);
  assert.match(rust, /clear_macro_save_block_if_file_missing\(&self\.file, &mut blocked_reason\)/);
  const start = rust.indexOf('fn save_macro_snapshot_to_file(');
  const end = rust.indexOf('impl MacroState {', start);
  assert.ok(start >= 0 && end > start, 'Macro save guard helper was not found');
  const saver = rust.slice(start, end);
  assert.ok(saver.indexOf('if clicks.is_empty()') < saver.indexOf('if let Some(reason) = blocked_reason'));
  assert.ok(saver.indexOf('if let Some(reason) = blocked_reason') < saver.indexOf('write_file_atomically('));
  const snapshotStart = rust.indexOf('fn save_macro_snapshot(&self');
  const snapshotEnd = rust.indexOf('fn block_macro_saves(&self', snapshotStart);
  assert.ok(snapshotStart >= 0 && snapshotEnd > snapshotStart, 'Macro snapshot method was not found');
  assert.match(rust.slice(snapshotStart, snapshotEnd), /if clicks\.is_empty\(\)\s*\{\s*return Ok\(0\);/);
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

test('Run History pin control is accessible and waits for its initial state', () => {
  assert.match(trackerHtml, /<button id="pin" title="Checking pin state…" disabled aria-label="Checking pin state">/);
  assert.match(trackerHtml, /pinStateReady=false/);
  assert.match(trackerHtml, /pinButton\.setAttribute\('aria-pressed',String\(pinned\)\)/);
  assert.match(trackerHtml, /pinButton\.disabled=false/);
  assert.match(trackerHtml, /if\(!pinStateReady\|\|pinButton\.disabled\)return/);
  assert.match(trackerHtml, /finally\{pinButton\.disabled=!pinStateReady\}/);
  assert.match(trackerHtml, /pinStatus\.className='status error'/);
});

test('Run History dashboard provides keyboard focus and accessible table semantics', () => {
  assert.match(trackerHtml, /button:focus-visible,input:focus-visible/);
  assert.match(trackerHtml, /<table aria-label="Completed dungeon rewards">/);
  assert.equal((trackerHtml.match(/<th scope="col">/g) || []).length, 9);
  assert.match(trackerHtml, /id="loadOlder" aria-controls="historyTable"/);
  assert.match(trackerHtml, /id="status" class="status" role="status" aria-live="polite"/);
});

test('Run History pin feedback is isolated and header controls can wrap', () => {
  assert.match(trackerHtml, /id="pinStatus" class="pin-status" role="status" aria-live="polite"/);
  assert.match(trackerHtml, /const pinStatus=document\.getElementById\('pinStatus'\)/);
  assert.match(trackerHtml, /function showPinError\(message\)\{pinStatus\.className='pin-status error'/);
  assert.match(trackerHtml, /pinStatus\.textContent=pinned\?'Window pinned\.'\:'Window unpinned\.'/);
  assert.match(trackerHtml, /header\{display:flex;flex-wrap:wrap/);
  assert.match(trackerHtml, /\.actions\{display:flex;flex-wrap:wrap/);
  assert.doesNotMatch(trackerHtml, /const pinStatus=document\.getElementById\('status'\)/);
});



test('dungeon heading observer ignores unrelated DOM mutations', () => {
  const injectionStart = rust.indexOf('const INJECTION_SCRIPT: &str = r#"');
  const injectionEnd = rust.indexOf('"#;', injectionStart);
  assert.ok(injectionStart >= 0 && injectionEnd > injectionStart,
    'Rust game-page injection script was not found');
  const injection = rust.slice(
    injectionStart + 'const INJECTION_SCRIPT: &str = r#"'.length,
    injectionEnd
  );
  const helperStart = injection.indexOf('const mutationAffectsDungeonHeading = (mutation) => {');
  const helperEnd = injection.indexOf('\n      };', helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart,
    'Dungeon heading mutation filter was not found');
  const helperSource = injection.slice(helperStart, helperEnd + '\n      };'.length);
  const mutationAffectsDungeonHeading = vm.runInNewContext(
    `(() => { ${helperSource}; return mutationAffectsDungeonHeading; })()`,
    {}
  );

  const ordinaryElement = {
    nodeType: 1,
    matches: () => false,
    closest: () => null,
    querySelector: () => null
  };
  const heading = {
    nodeType: 1,
    matches: (selector) => selector === 'h1',
    closest: (selector) => selector === 'h1' ? heading : null,
    querySelector: () => null
  };
  const headingChild = {
    nodeType: 1,
    matches: () => false,
    closest: (selector) => selector === 'h1' ? heading : null,
    querySelector: () => null
  };
  const insertedContainer = {
    nodeType: 1,
    matches: () => false,
    closest: () => null,
    querySelector: (selector) => selector === 'h1' ? heading : null
  };

  assert.equal(mutationAffectsDungeonHeading({
    type: 'childList', target: ordinaryElement, addedNodes: [ordinaryElement], removedNodes: []
  }), false, 'Unrelated page content must not schedule a heading capture');
  assert.equal(mutationAffectsDungeonHeading({
    type: 'childList', target: heading, addedNodes: [], removedNodes: []
  }), true, 'Replacing the H1 contents must schedule a heading capture');
  assert.equal(mutationAffectsDungeonHeading({
    type: 'childList', target: headingChild, addedNodes: [], removedNodes: []
  }), true, 'Changing content inside the H1 must schedule a heading capture');
  assert.equal(mutationAffectsDungeonHeading({
    type: 'childList', target: ordinaryElement, addedNodes: [insertedContainer], removedNodes: []
  }), true, 'Inserting a subtree containing an H1 must schedule a heading capture');
  assert.equal(mutationAffectsDungeonHeading({
    type: 'childList', target: ordinaryElement, addedNodes: [], removedNodes: [heading]
  }), true, 'Removing the H1 must schedule a heading capture');

  assert.ok(injection.includes('new MutationObserver((mutations) => {'),
    'The observer must inspect mutation records');
  assert.ok(injection.includes('!mutations.some(mutationAffectsDungeonHeading)'),
    'The observer must filter mutation records before scheduling the debounced capture');
  assert.ok(injection.includes('}, 120);'),
    'The existing 120 ms debounce must be preserved');
  assert.ok(injection.includes('headingObserver.observe(document.documentElement, { childList: true, subtree: true })'),
    'The existing child-list observer coverage must remain unchanged');
});


test('Run History rendering preserves newest-first order, filtering, and escaping', () => {
  const helperStart = trackerHtml.indexOf('function historyRowsHtml(');
  const helperEnd = trackerHtml.indexOf('\n}', helperStart);
  assert.ok(helperStart >= 0 && helperEnd > helperStart, 'Run History row renderer was not found');
  const helperSource = trackerHtml.slice(helperStart, helperEnd + 2);
  const context = {
    elapsedTimeText: () => '1m 2s',
    esc: (value) => String(value ?? '').replace(/[&<>"']/g, (character) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
    })[character]),
    timeText: (value) => value ? `time-${value}` : '—',
    fmt: (value) => String(Number(value || 0))
  };
  const historyRowsHtml = vm.runInNewContext(
    `(() => { ${helperSource}; return historyRowsHtml; })()`,
    context
  );
  const records = [
    { display_name: 'First Dungeon', kind: 'Single run', timestamp_unix_ms: 1,
      values: { runs: 1, zel: 10, karma: 2, honor: 3, exp: 4 } },
    { display_name: 'Second Dungeon', kind: 'Repeat batch', timestamp_unix_ms: 2,
      values: { runs: 5, zel: 50, karma: 6, honor: 7, exp: 8 } },
    { display_name: '<Third & Final>', kind: 'Single run', timestamp_unix_ms: 3,
      values: { runs: 1, zel: 30, karma: 4, honor: 5, exp: 6 } }
  ];

  const allRows = historyRowsHtml(records, '', 'No rows');
  assert.ok(allRows.indexOf('Third &amp; Final') < allRows.indexOf('Second Dungeon'));
  assert.ok(allRows.indexOf('Second Dungeon') < allRows.indexOf('First Dungeon'));
  assert.equal((allRows.match(/<tr>/g) || []).length, 3);
  assert.match(allRows, /&lt;Third &amp; Final&gt;/);
  assert.match(allRows, /time-3/);

  const filtered = historyRowsHtml(records, 'second', 'No matching reward records.');
  assert.equal((filtered.match(/<tr>/g) || []).length, 1);
  assert.match(filtered, /Second Dungeon/);
  assert.doesNotMatch(filtered, /First Dungeon|Third/);

  const empty = historyRowsHtml(records, 'missing', 'No matching reward records.');
  assert.match(empty, /No matching reward records/);
  assert.match(empty, /colspan="9"/);
  assert.ok(trackerHtml.includes('historyRowsHtml(rewards,q,emptyMessage)'), 'Dashboard render must use the optimized row renderer');
  assert.ok(!trackerHtml.includes('rewards.slice().reverse().filter('), 'Dashboard render must not recreate reversed and filtered reward arrays');
});


test('macro capture accepts only trusted left clicks and normalizes viewport coordinates', async () => {
  const injectionMatch = rust.match(/const INJECTION_SCRIPT: &str = r#"([\s\S]*?)"#;/);
  assert.ok(injectionMatch, 'Rust game-page injection script was not found');
  const source = injectionMatch[1];
  const start = source.indexOf('// Serialize IPC calls so rapid physical clicks cannot be stored out of order.');
  const end = source.indexOf('        }, true);', start);
  assert.ok(start >= 0 && end > start, 'Physical click capture handler was not found');
  const captureSource = source.slice(start, end + '        }, true);'.length);

  let pointerHandler;
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
      return Promise.resolve();
    }
  };

  vm.runInNewContext(captureSource, context);
  assert.equal(typeof pointerHandler, 'function', 'Physical pointer handler must be registered');

  pointerHandler({ isTrusted: false, button: 0, clientX: 500, clientY: 400 });
  pointerHandler({ isTrusted: true, button: 2, clientX: 500, clientY: 400 });
  assert.equal(calls.length, 0, 'Synthetic and non-left pointer events must be ignored');

  pointerHandler({ isTrusted: true, button: 0, clientX: 250, clientY: 400 });
  pointerHandler({ isTrusted: true, button: 0, clientX: -10, clientY: 810 });
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(calls.map((call) => call.command), ['record_click', 'record_click']);
  assert.equal(
    JSON.stringify(calls.map((call) => call.payload)),
    JSON.stringify([{ x: 0.25, y: 0.5 }, { x: 0, y: 1 }]),
    'Accepted clicks must be normalized to the viewport and clamped at its edges'
  );
});
