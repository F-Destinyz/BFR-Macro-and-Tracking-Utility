use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    fs,
    io::{BufRead, Read, Seek, SeekFrom, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{
    image::Image,
    menu::{IconMenuItem, IsMenuItem, Menu, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder, Wry,
};
use tauri_plugin_notification::NotificationExt;
use tungstenite::{Message, WebSocket};
use url::Url;

const GAME_URL: &str = "https://rebornbf.com/home";
const WINDOW_WIDTH: f64 = 900.0;
const WINDOW_HEIGHT: f64 = 1125.0;
const LOOP_GAP: Duration = Duration::from_millis(500);
const TRACKER_DIAGNOSTIC_WINDOW: Duration = Duration::from_secs(60);
const MAX_TRACKER_DIAGNOSTICS_PER_WINDOW: u32 = 120;
const MAX_RECORDING_ERROR_MESSAGE_CHARS: usize = 500;
const CDP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CDP_IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MACRO_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MACRO_CLICKS: usize = 100_000;
const MAX_MACRO_DURATION_MS: u64 = 24 * 60 * 60 * 1000;
const MODE_IDLE: u8 = 0;
const MODE_RECORDING: u8 = 1;
const MODE_PLAYING: u8 = 2;
const MODE_STOPPING: u8 = 3;

// Write beside the destination, flush the temporary file, then replace the old
// file. Keeping the temporary file on the same volume allows replacement to be
// performed by the operating system without exposing a partially-written JSON.
fn write_file_atomically(destination: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::sync::atomic::AtomicU64;
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let parent = destination
        .parent()
        .ok_or_else(|| "File destination has no parent directory".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data");
    let mut temporary = None;
    for _ in 0..10 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(&candidate);
                    return Err(error.to_string());
                }
                drop(file);
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    let temporary =
        temporary.ok_or_else(|| "Could not create a temporary save file".to_string())?;
    let result = replace_file(&temporary, destination);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let success = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if success == 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination).map_err(|error| error.to_string())
}

// This runs in the remote game page. Tauri's remote capability is restricted to
// rebornbf.com and exposes only the explicitly listed game-bridge commands.
// Only trusted physical left-button pointer presses are recorded.
const INJECTION_SCRIPT: &str = r#"
(() => {
  if (window.__bfrMacroCaptureInstalled || window.__bfrMacroCaptureConnecting) return;
  window.__bfrMacroCaptureConnecting = true;

  const startedAt = Date.now();
  let lastError = '';

  const connectBridge = () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') {
      if (Date.now() - startedAt < 30000) {
        setTimeout(connectBridge, 150);
      } else {
        window.__bfrMacroCaptureConnecting = false;
        console.error('[BFR Macro] Tauri IPC bridge did not become available within 30 seconds.');
      }
      return;
    }

    invoke('recording_bridge_ready')
      .then(() => {
        if (window.__bfrMacroCaptureInstalled) return;
        window.__bfrMacroCaptureInstalled = true;
        window.__bfrMacroCaptureConnecting = false;
        // Serialize IPC calls so rapid physical clicks cannot be stored out of order.
        // Bound queued work so a stalled WebView2 bridge cannot grow memory indefinitely.
        let captureQueue = Promise.resolve();
        let captureQueueDepth = 0;
        const MAX_CAPTURE_QUEUE_DEPTH = 512;
        let captureFailed = false;
        document.addEventListener('pointerdown', (event) => {
          if (!event.isTrusted || event.button !== 0 || captureFailed) return;
          if (captureQueueDepth >= MAX_CAPTURE_QUEUE_DEPTH) {
            captureFailed = true;
            console.error('[BFR Macro] Click capture queue exceeded its safety limit.');
            invoke('report_recording_error', {
              message: 'Click capture queue exceeded its safety limit; recording was stopped to protect application memory.'
            }).catch((reportError) => {
              console.error('[BFR Macro] Could not report recording queue overflow:', reportError);
            });
            return;
          }
          const width = Math.max(1, window.innerWidth);
          const height = Math.max(1, window.innerHeight);
          const x = Math.min(1, Math.max(0, event.clientX / width));
          const y = Math.min(1, Math.max(0, event.clientY / height));
          captureQueueDepth += 1;
          captureQueue = captureQueue.then(() => {
            if (captureFailed) return;
            return invoke('record_click', { x, y });
          }).catch((error) => {
            if (captureFailed) return;
            captureFailed = true;
            console.error('[BFR Macro] Click capture IPC failed:', error);
            return invoke('report_recording_error', { message: String(error) }).catch((reportError) => {
              console.error('[BFR Macro] Could not report recording error:', reportError);
            });
          }).finally(() => {
            captureQueueDepth = Math.max(0, captureQueueDepth - 1);
          });
        }, true);
        console.info('[BFR Macro] Physical click capture bridge connected.');
      })
      .catch((error) => {
        lastError = String(error);
        console.error('[BFR Macro] Recording bridge handshake failed; retrying:', error);
        if (Date.now() - startedAt < 30000) {
          setTimeout(connectBridge, 500);
        } else {
          window.__bfrMacroCaptureConnecting = false;
          console.error('[BFR Macro] Recording bridge failed after 30 seconds:', lastError);
        }
      });
  };

  connectBridge();

  // Background reward tracker. It reads only the two known battle reward
  // responses and the active dungeon heading; unrelated network traffic is ignored.
  const startTrackerProbe = () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function' || !document.documentElement || window.__bfrTrackerProbeInstalled || window.__bfrTrackerProbeStarting) return false;
    window.__bfrTrackerProbeStarting = true;
    try {
    const pageOrigin = window.location.origin;
    let diagnosticQueue = Promise.resolve();
    const report = (event, details) => {
      diagnosticQueue = diagnosticQueue.then(() => invoke('tracker_diagnostic_event', { event, details }))
        .catch((error) => {
          console.error('[BFR Tracker] Diagnostic log IPC failed:', event, String(error));
        });
      return diagnosticQueue;
    };

    report('observer_started', {
      page_host: window.location.hostname,
      page_path: window.location.pathname,
      observer_version: '2.0.0',
      frame_is_top: window.top === window
    });

    // Track the most recently visited dungeon route across SPA navigation.
    // Names are optional labels supplied by the user; unknown dungeon IDs are
    // still tracked automatically without requiring a per-dungeon code change.
    // Never infer a quest name from its parent dungeon page. The /start/{id}
    // route is the authoritative run identifier; unknown IDs remain visible
    // until the user confirms their names.
    let activeDungeonRoute = '';
    let activeDungeonId = '';
    let activeDungeonName = '';
    let activeDungeonStartedAt = 0;
    let activeDungeonStartSource = '';
    let lastReportedDungeonContext = '';
    const reportDungeonContext = (source) => {
      const signature = `${activeDungeonRoute}|${activeDungeonId}|${activeDungeonName}|${activeDungeonStartedAt}|${activeDungeonStartSource}|${activeRun}|${activeRunType}`;
      if (!activeDungeonRoute || signature === lastReportedDungeonContext) return;
      lastReportedDungeonContext = signature;
      const context = {
        host: window.location.hostname,
        route: activeDungeonRoute,
        dungeon_id: activeDungeonId,
        dungeon_name: activeDungeonName,
        dungeon_started_at_unix_ms: activeDungeonStartedAt,
        dungeon_start_source: activeDungeonStartSource,
        run_active: activeRun,
        run_type: activeRunType,
        source
      };
      const invoke = window.__TAURI_INTERNALS__?.invoke;
      if (typeof invoke === 'function') {
        invoke('tracker_dungeon_context', { details: context }).catch((error) => {
          console.error('[BFR Tracker] Could not save dungeon context:', String(error));
          report('observer_error', { stage: 'dungeon_context_persistence', message: String(error).slice(0, 180) });
        });
      }
      report('dungeon_context_seen', context);
    };
    let activeRun = false;
    let activeRunType = 'single';
    let beginQuestClickAt = 0;
    let beginQuestClickDungeonId = '';
    // The /begin route is a consequence of starting a quest, not proof that
    // the player clicked Begin Quest. Capture the actual UI click in the page
    // before navigation so the timer cannot start on the preview/setup screen.
    const captureQuestActionClick = (event) => {
      if (!event.isTrusted || (event.button !== 0 && event.detail !== 0)) return;
      const pathName = window.location.pathname;
      const routeMatch = pathName.match(/^\/start\/(\d+)(?:$|\/)/);
      const dungeonRouteMatch = pathName.match(/^\/dungeons\/(?:vortex|area)\/(\d+)(?:$|\/)/);
      // Repeat controls can appear on the quest start page or the battle
      // result page, not only on the dungeon-selection route. On /battle,
      // reuse the already-confirmed active dungeon ID instead of guessing.
      const repeatRouteMatch = routeMatch || dungeonRouteMatch;
      const repeatDungeonId = repeatRouteMatch?.[1]
        || (/^\/battle(?:$|\/)/.test(pathName) ? activeDungeonId : '');
      const path = typeof event.composedPath === 'function' ? event.composedPath() : [];
      const candidates = path.length ? path : [event.target];
      for (const node of candidates) {
        if (!node || node.nodeType !== 1 || typeof node.matches !== 'function') continue;
        const tag = String(node.tagName || '').toLowerCase();
        if (tag === 'html' || tag === 'body') continue;
        const interactive = node.matches('button, [role="button"], a, input[type="submit"], [tabindex]:not([tabindex="-1"]), [onclick], [data-action]');
        // Use textContent for non-interactive wrappers: innerText can force a
        // layout/reflow on large game containers during every trusted click.
        const ownText = String(interactive ? (node.innerText || node.textContent || '') : (node.textContent || '')).replace(/\s+/g, ' ').trim().slice(0, 120);
        const label = [
          node.getAttribute('aria-label') || '',
          node.getAttribute('title') || '',
          node.getAttribute('data-action') || '',
          ownText,
          node.value || ''
        ].join(' ').replace(/\s+/g, ' ').trim().slice(0, 240);
        const isBeginQuest = /\bbegin\s+quest\b/i.test(label);
        // The live game may label this control simply "Repeat" or "Repeat 10".
        // The batch ID in the API response remains authoritative for run type.
        const isRepeatAction = /\brepeat\b/i.test(label);
        const exactActionWrapper = ownText.length <= 100 && /^(?:begin\s+quest|repeat(?:\s+(?:batch|battles?))?(?:\s*(?:x|×)?\s*\(?\d+\)?\s*(?:x|×)?)?|batch\s+repeat)(?:\s*\(?\d+\)?)?$/i.test(ownText);
        if (!interactive && !exactActionWrapper) continue;

        if (isBeginQuest && routeMatch) {
          const startedAt = Date.now();
          beginQuestClickAt = startedAt;
          beginQuestClickDungeonId = routeMatch[1];
          activeRun = true;
          activeRunType = 'single';
          activeDungeonId = routeMatch[1];
          activeDungeonName = String(document.querySelector('h1')?.textContent || '').trim();
          activeDungeonStartedAt = startedAt;
          activeDungeonStartSource = 'begin_quest_click';
          activeDungeonRoute = `/start/${routeMatch[1]}/begin`;
          reportDungeonContext('begin_quest_click');
          report('begin_quest_click_seen', {
            dungeon_id: beginQuestClickDungeonId,
            route: pathName,
            element_tag: tag,
            frame_is_top: window.top === window,
            started_at_unix_ms: startedAt
          });
          break;
        }

        if (isRepeatAction && repeatDungeonId) {
          const startedAt = Date.now();
          const dungeonId = repeatDungeonId;
          const actionKind = /\brepeat\s+batch\b|\bbatch\s+repeat\b/i.test(label)
            ? 'batch'
            : /\brepeat\s+battles?\b/i.test(label) ? 'battles' : 'generic';
          beginQuestClickAt = startedAt;
          beginQuestClickDungeonId = dungeonId;
          activeRun = true;
          activeRunType = actionKind === 'batch' ? 'repeat_batch' : 'repeat';
          activeDungeonId = dungeonId;
          // Only replace the quest name when the click happens on a dungeon
          // or start route. The /battle heading is not necessarily the quest.
          if (pathName.startsWith('/start/') || pathName.startsWith('/dungeons/')) {
            const heading = String(document.querySelector('h1')?.textContent || '').trim();
            if (heading) activeDungeonName = heading;
          }
          activeDungeonStartedAt = startedAt;
          activeDungeonStartSource = 'repeat_action_click';
          activeDungeonRoute = dungeonRouteMatch
            ? pathName.split('?')[0].split('#')[0]
            : `/start/${dungeonId}/begin`;
          reportDungeonContext('repeat_action_click');
          report('repeat_action_click_seen', {
            dungeon_id: dungeonId,
            route: pathName,
            action_kind: actionKind,
            element_tag: tag,
            frame_is_top: window.top === window,
            started_at_unix_ms: startedAt
          });
          break;
        }
      }
    };

    const updateDungeonContext = (pathValue) => {
      const path = String(pathValue || '').split('?')[0].split('#')[0];
      const startMatch = path.match(/^\/start\/(\d+)\/begin(?:$|\/)/);
      if (startMatch) {
        // Confirm the route only after the actual Begin Quest click has been
        // captured. Never start the timer from route observation alone.
        if (!activeRun || beginQuestClickDungeonId !== startMatch[1] || beginQuestClickAt <= 0) {
          activeRun = false;
          activeDungeonStartedAt = 0;
          activeDungeonStartSource = '';
          activeDungeonName = '';
          activeDungeonId = '';
          activeDungeonRoute = '';
          activeRunType = 'single';
          return;
        }
        activeDungeonId = startMatch[1];
        activeDungeonRoute = `/start/${startMatch[1]}/begin`;
        reportDungeonContext('begin_quest_route_confirmed');
        return;
      }
      // /start/{id} is only the quest preview/setup screen. Do not create a
      // run, assign a timer, or carry an old quest into this page.
      if (/^\/start\/\d+(?:$|\/)/.test(path) && !startMatch) {
        if (!activeRun) {
          activeDungeonStartedAt = 0;
          activeDungeonStartSource = '';
          activeDungeonId = '';
          activeDungeonName = '';
          activeDungeonRoute = '';
          beginQuestClickAt = 0;
          beginQuestClickDungeonId = '';
          activeRunType = 'single';
        }
        return;
      }
      // The battle route is an intermediate screen between quest start and
      // reward settlement. Preserve the active run context until /finish or
      // a completed /repeat response arrives; otherwise reward records lose
      // their ID, name and start timestamp.
      if (path === '/battle' && activeRun) return;

      const dungeonSelection = path.match(/^\/dungeons\/(?:vortex|area)\/(\d+)(?:$|\/)/);
      if (dungeonSelection) {
        // After Begin Quest, the game may navigate from /start/{id} into the
        // actual dungeon route. Keep that run active and retain its exact ID.
        if (activeRun) return;
        activeRun = false;
        activeDungeonStartedAt = 0;
        activeDungeonStartSource = '';
        activeDungeonId = '';
        activeDungeonName = '';
        activeDungeonRoute = path;
        beginQuestClickAt = 0;
        beginQuestClickDungeonId = '';
        activeRunType = 'single';
        reportDungeonContext('dungeon_selection_idle');
        return;
      }
      if (activeRun) {
        activeRun = false;
        activeDungeonStartedAt = 0;
        activeDungeonStartSource = '';
        activeDungeonId = '';
        activeDungeonName = '';
        activeDungeonRoute = path;
        reportDungeonContext('left_run');
        activeRunType = 'single';
        beginQuestClickAt = 0;
        beginQuestClickDungeonId = '';
        activeRunType = 'single';
      }
    };
    updateDungeonContext(window.location.pathname);
    let captureDungeonHeadings = () => {};
    const notifyRouteChange = () => {
      updateDungeonContext(window.location.pathname);
      captureDungeonHeadings();
    };
    for (const methodName of ['pushState', 'replaceState']) {
      const original = history[methodName];
      history[methodName] = function(...args) {
        const result = original.apply(this, args);
        notifyRouteChange();
        return result;
      };
    }
    window.addEventListener('popstate', notifyRouteChange);

    const numericRewardObject = (value) => {
      if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
      const result = {};
      for (const [key, amount] of Object.entries(value).slice(0, 200)) {
        if (!/^[A-Za-z0-9_-]{1,80}$/.test(key)) continue;
        if (typeof amount === 'number' && Number.isFinite(amount) && amount >= 0) result[key] = amount;
      }
      return result;
    };
    const numericRewardFields = (value) => {
      const result = {};
      if (!value || typeof value !== 'object') return result;
      for (const key of ['exp', 'zel', 'gems', 'runs', 'wins', 'honor', 'karma', 'level', 'settled_runs', 'auto_sell_zel']) {
        const amount = value[key];
        if (typeof amount === 'number' && Number.isFinite(amount) && amount >= 0) result[key] = amount;
      }
      return result;
    };

    const inspectRepeatResponse = (urlValue, method, status, contentType, payload) => {
      let parsedUrl;
      try { parsedUrl = new URL(urlValue, pageOrigin); } catch (_) { return; }
      if (parsedUrl.origin !== pageOrigin || parsedUrl.pathname !== '/api/battles/repeat' || Number(status) < 200 || Number(status) >= 300) return;
      if (!activeDungeonRoute) {
        const currentPath = window.location.pathname;
        if (currentPath.startsWith('/dungeons/') || currentPath.startsWith('/start/')) updateDungeonContext(currentPath);
      }
      const settlement = payload?.settlement || {};
      const totals = payload?.totals || {};
      const batchId = typeof payload?.batch_id === 'string' ? payload.batch_id : (typeof totals.batch_id === 'string' ? totals.batch_id : (typeof settlement.batch_id === 'string' ? settlement.batch_id : ''));
      // The repeat endpoint is authoritative. A batch ID identifies a repeat
      // batch even when the UI click listener missed the game's custom wrapper.
      const repeatRunType = batchId ? 'repeat_batch' : 'repeat';
      const currentStartMatch = window.location.pathname.match(/^\/start\/(\d+)\/begin(?:$|\/)/);
      const currentDungeonMatch = window.location.pathname.match(/^\/dungeons\/(?:vortex|area)\/(\d+)(?:$|\/)/);
      if (currentStartMatch && (!activeDungeonId || activeDungeonId !== currentStartMatch[1])) {
        activeDungeonId = currentStartMatch[1];
        activeDungeonRoute = `/start/${currentStartMatch[1]}/begin`;
        activeDungeonName = String(document.querySelector('h1')?.textContent || '').trim();
      } else if (currentDungeonMatch && !activeDungeonId) {
        // The selection route carries the run ID, but its heading may be a
        // parent dungeon name. Keep the ID and wait for a quest-specific name.
        activeDungeonId = currentDungeonMatch[1];
        activeDungeonRoute = window.location.pathname.split('?')[0].split('#')[0];
        activeDungeonName = '';
      }
      // The Begin Quest click can be captured before the game reveals that
      // this run is a repeat batch. In that case the click timestamp is still
      // the correct start of the batch; changing the run type must not reset it
      // to the time the reward response arrives.
      const hasValidRunStart = activeRun
        && activeDungeonStartedAt > 0
        && (!activeDungeonId || !currentDungeonMatch || activeDungeonId === currentDungeonMatch[1]);
      if (!hasValidRunStart) {
        // The custom action click was missed (or belongs to another dungeon).
        // Use the first successful repeat response as a fallback start point.
        activeDungeonStartedAt = Date.now();
        activeDungeonStartSource = 'api_fallback';
      }
      activeRun = true;
      activeRunType = repeatRunType;
      if (!activeDungeonRoute && activeDungeonId) activeDungeonRoute = `/start/${activeDungeonId}/begin`;
      reportDungeonContext('repeat_api_confirmed');
      // settlement.finished describes the current repeat settlement chunk.
      // Only totals.finished confirms that the overall repeat batch has ended.
      const batchFinished = totals.finished === true;
      // Unit arrays are intentionally not counted as drops: discovery showed
      // they can represent a changing inventory snapshot. We only record their
      // observed length until their semantics are proven.
      reportReward({
        host: parsedUrl.hostname, path: parsedUrl.pathname,
        dungeon_route: activeDungeonRoute, dungeon_id: activeDungeonId, dungeon_name: activeDungeonName,
        batch_id: batchId, dungeon_started_at_unix_ms: activeDungeonStartedAt, dungeon_start_source: activeDungeonStartSource, run_type: repeatRunType,
        settlement: { ...numericRewardFields(settlement), finished: settlement.finished === true },
        totals: { ...numericRewardFields(totals), finished: totals.finished === true },
        quest_finished: batchFinished,
        items: numericRewardObject(settlement.items),
        totals_items: numericRewardObject(totals.items)
      });
      if (batchFinished && activeRun) {
        activeRun = false;
        reportDungeonContext('quest_completed');
        beginQuestClickAt = 0;
        beginQuestClickDungeonId = '';
        activeRunType = 'single';
      }
    };

    // Normal one-run battles settle through /api/battles/finish. The repeat
    // endpoint above has a different response shape (settlement + totals), so
    // normalize the finish response's rewards object into the same safe record.
    const inspectBattleFinishResponse = (urlValue, method, status, contentType, payload) => {
      let parsedUrl;
      try { parsedUrl = new URL(urlValue, pageOrigin); } catch (_) { return; }
      if (parsedUrl.origin !== pageOrigin || parsedUrl.pathname !== '/api/battles/finish'
          || Number(status) < 200 || Number(status) >= 300) return;
      const rewards = payload?.rewards;
      if (!rewards || typeof rewards !== 'object' || Array.isArray(rewards) || payload?.ok === false) return;
      const currentPath = window.location.pathname;
      const finishStartMatch = currentPath.match(/^\/start\/(\d+)\/begin(?:$|\/)/);
      if (finishStartMatch) {
        activeDungeonId = finishStartMatch[1];
        activeDungeonRoute = `/start/${finishStartMatch[1]}/begin`;
        activeDungeonName = String(document.querySelector('h1')?.textContent || activeDungeonName || '').trim();
      } else if (!activeDungeonId && beginQuestClickDungeonId) {
        activeDungeonId = beginQuestClickDungeonId;
        activeDungeonRoute = `/start/${beginQuestClickDungeonId}/begin`;
      } else if (!activeDungeonRoute && (currentPath.startsWith('/dungeons/') || currentPath.startsWith('/start/'))) {
        updateDungeonContext(currentPath);
      }
      const currentRewards = payload.rewards;
      reportReward({
        host: parsedUrl.hostname, path: parsedUrl.pathname,
        dungeon_route: activeDungeonRoute, dungeon_id: activeDungeonId, dungeon_name: activeDungeonName,
        batch_id: '', dungeon_started_at_unix_ms: activeDungeonStartedAt, dungeon_start_source: activeDungeonStartSource, run_type: activeRunType,
        settlement: { ...numericRewardFields({ ...currentRewards, runs: 1 }), finished: true },
        totals: {}, quest_finished: true, items: numericRewardObject(currentRewards.items), totals_items: {}
      });
      if (activeRun) {
        activeRun = false;
        reportDungeonContext('quest_completed');
        beginQuestClickAt = 0;
        beginQuestClickDungeonId = '';
        activeRunType = 'single';
      }
    };

    const reportReward = (details) => {
      const invoke = window.__TAURI_INTERNALS__?.invoke;
      if (typeof invoke !== 'function') {
        console.error('[BFR Tracker] Tauri invoke bridge unavailable; reward was not persisted.');
        return;
      }
      invoke('tracker_reward_event', { details }).then((persisted) => {
        if (!persisted) return;
        invoke('tracker_diagnostic_event', { event: 'reward_persisted', details: {
          dungeon_id: String(details.dungeon_id || '').slice(0, 40),
          settled_runs: Number(details.settlement?.settled_runs || 0)
        }}).catch(() => {});
      }).catch((error) => {
        console.error('[BFR Tracker] Could not persist reward event:', String(error));
        invoke('tracker_diagnostic_event', { event: 'reward_persist_failed', details: {
          stage: 'reward_persistence', message: String(error).slice(0, 180)
        }}).catch(() => {});
      });
    };

    // Read only the visible H1 on dungeon routes to resolve the active quest name.
    // Do not inspect general page text, forms, or account widgets.
    captureDungeonHeadings = () => {
      const path = window.location.pathname;
      updateDungeonContext(path);
      if (!(path.startsWith('/dungeons/') || path.startsWith('/start/'))) return;
      const heading = document.querySelector('h1');
      const label = String(heading?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 100);
      if (/^\/start\/\d+\/begin(?:$|\/)/.test(path)
          && label && label.toLowerCase() !== 'reinforcement'
          && activeRun && activeDungeonName !== label) {
        activeDungeonName = label;
        activeDungeonRoute = path.split('/').slice(0, 4).join('/');
        reportDungeonContext('quest_name_detected');
      }
    };
    captureDungeonHeadings();
    if (!window.__bfrDungeonHeadingObserver && typeof MutationObserver !== 'undefined') {
      let headingCaptureScheduled = false;
      const headingObserver = new MutationObserver(() => {
        if (headingCaptureScheduled) return;
        headingCaptureScheduled = true;
        window.setTimeout(() => {
          headingCaptureScheduled = false;
          captureDungeonHeadings();
        }, 120);
      });
      headingObserver.observe(document.documentElement, { childList: true, subtree: true });
      // Publish the global reference only after observe() succeeds, so a failed
      // startup retry cannot mistake an unconnected observer for a live one.
      window.__bfrDungeonHeadingObserver = headingObserver;
    }

    if (!window.__bfrTrackerFetchWrapped && typeof window.fetch === 'function') {
      window.__bfrTrackerFetchWrapped = true;
      const originalFetch = window.fetch;
      window.fetch = function(...args) {
        const requestUrl = args[0] instanceof Request ? args[0].url : String(args[0] || '');
        const requestMethod = (args[1] && args[1].method) || (args[0] instanceof Request ? args[0].method : 'GET');
        return originalFetch.apply(this, args).then((response) => {
          try {
            const parsed = new URL(response.url || requestUrl, pageOrigin);
            if (parsed.origin !== pageOrigin) return response;
            if (parsed.pathname.startsWith('/dungeons/') || parsed.pathname.startsWith('/start/')) {
              updateDungeonContext(parsed.pathname);
              captureDungeonHeadings();
            }
            if (parsed.pathname === '/api/battles/repeat' || parsed.pathname === '/api/battles/finish') {
              const contentType = response.headers.get('content-type') || '';
              if (contentType.toLowerCase().includes('json')) {
                response.clone().json().then((payload) => {
                  if (parsed.pathname === '/api/battles/repeat') inspectRepeatResponse(parsed.href, requestMethod, response.status, contentType, payload);
                  else inspectBattleFinishResponse(parsed.href, requestMethod, response.status, contentType, payload);
                }).catch(() => {});
              }
            }
          } catch (_) {}
          return response;
        });
      };
    }

    if (!window.__bfrTrackerXhrWrapped && typeof XMLHttpRequest !== 'undefined') {
      window.__bfrTrackerXhrWrapped = true;
      const originalOpen = XMLHttpRequest.prototype.open;
      const originalSend = XMLHttpRequest.prototype.send;
      XMLHttpRequest.prototype.open = function(method, url, ...rest) {
        this.__bfrTrackerMethod = method;
        this.__bfrTrackerUrl = String(url || '');
        return originalOpen.call(this, method, url, ...rest);
      };
      XMLHttpRequest.prototype.send = function(...args) {
        this.addEventListener('load', function() {
          try {
            const parsed = new URL(this.responseURL || this.__bfrTrackerUrl, pageOrigin);
            if (parsed.origin !== pageOrigin) return;
            if (parsed.pathname.startsWith('/dungeons/') || parsed.pathname.startsWith('/start/')) {
              updateDungeonContext(parsed.pathname);
              captureDungeonHeadings();
              return;
            }
            if (parsed.pathname !== '/api/battles/repeat' && parsed.pathname !== '/api/battles/finish') return;
            const contentType = this.getResponseHeader('content-type') || '';
            if (!contentType.toLowerCase().includes('json')) return;
            let payload = this.responseType === 'json' ? this.response : this.responseText;
            if (typeof payload === 'string') payload = JSON.parse(payload);
            if (parsed.pathname === '/api/battles/repeat') inspectRepeatResponse(parsed.href, this.__bfrTrackerMethod, this.status, contentType, payload);
            else inspectBattleFinishResponse(parsed.href, this.__bfrTrackerMethod, this.status, contentType, payload);
          } catch (_) {}
        }, { once: true });
        return originalSend.apply(this, args);
      };
    }

    // Only the functional reward observers and the narrowly-scoped dungeon
    // heading observer remain active. Broad network, WebSocket, resource and
    // whole-page mutation discovery instrumentation has been retired.
    // Attach the click listener last. If any earlier setup step fails, the
    // retry can safely rebuild the tracker without leaving a stale listener
    // behind. This prevents one physical click from being reported twice.
    if (!window.__bfrTrackerActionClickListenerInstalled) {
      document.addEventListener('click', captureQuestActionClick, true);
      window.__bfrTrackerActionClickListenerInstalled = true;
    }
    report('observer_status', { component: 'tracker', status: 'installed', page_path: location.pathname });
    window.__bfrTrackerProbeInstalled = true;
    window.__bfrTrackerProbeStarting = false;
    console.info('[BFR Tracker] Global observer installed successfully.');
    return true;
    } catch (error) {
      window.__bfrTrackerProbeStarting = false;
      const invoke = window.__TAURI_INTERNALS__?.invoke;
      if (typeof invoke === 'function') {
        invoke('tracker_diagnostic_event', { event: 'observer_error', details: {
          stage: 'tracker_initialization',
          message: String(error).slice(0, 180)
        }}).catch(() => {});
      }
      console.error('[BFR Tracker] Global observer initialization failed; retry will continue:', error);
      return false;
    }
  };

  // Restore the earlier document-start lifecycle: start independently from
  // macro recording and retry while the Tauri IPC bridge becomes available.
  if (!startTrackerProbe()) {
    let attempts = 0;
    const retry = () => {
      attempts += 1;
      if (window.__bfrTrackerProbeInstalled || window.__bfrTrackerProbeStarting) return;
      if (startTrackerProbe()) return;
      if (attempts < 200) window.__bfrTrackerProbeRetry = setTimeout(retry, 300);
      else console.error('[BFR Tracker] Observer could not start: Tauri IPC bridge unavailable after 60 seconds.');
    };
    window.__bfrTrackerProbeRetry = setTimeout(retry, 300);
  }
})();
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MacroClick {
    x: f64,
    y: f64,
    elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MacroFile {
    format: String,
    clicks: Vec<MacroClick>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct AppSettings {
    width: f64,
    height: f64,
    pinned: bool,
    tracker_pinned: bool,
    tracker_width: f64,
    tracker_height: f64,
    loop_enabled: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            width: WINDOW_WIDTH,
            height: WINDOW_HEIGHT,
            pinned: false,
            tracker_pinned: false,
            tracker_width: 760.0,
            tracker_height: 820.0,
            loop_enabled: true,
        }
    }
}

struct SettingsState {
    file: PathBuf,
    settings: Mutex<AppSettings>,
    save_lock: Mutex<()>,
    // If existing settings could not be read safely, refuse writes so defaults
    // cannot overwrite the user's original file before manual recovery.
    save_blocked_reason: Option<String>,
    resize_save_tx: mpsc::Sender<()>,
}

impl SettingsState {
    fn save(&self) -> Result<(), String> {
        if let Some(reason) = &self.save_blocked_reason {
            return Err(format!(
                "Settings were not saved to protect the existing settings file: {reason}"
            ));
        }
        // Serialize save operations so concurrent tray/resize saves cannot overwrite
        // a newer snapshot with an older one.
        let _save_guard = self
            .save_lock
            .lock()
            .map_err(|_| "Settings save operation is unavailable".to_string())?;
        // Snapshot settings under the data lock, then perform serialization and disk I/O
        // without holding that lock. This keeps tray and resize interactions responsive.
        let snapshot = self
            .settings
            .lock()
            .map_err(|_| "Settings are unavailable because their lock was poisoned".to_string())?
            .clone();
        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|error| error.to_string())?;
        write_file_atomically(&self.file, &bytes)
    }
}

#[derive(Debug, Default)]
struct TrackerDiagnosticRateLimit {
    accepted_at: VecDeque<Instant>,
}

impl TrackerDiagnosticRateLimit {
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        while self
            .accepted_at
            .front()
            .is_some_and(|accepted| now.duration_since(*accepted) >= TRACKER_DIAGNOSTIC_WINDOW)
        {
            self.accepted_at.pop_front();
        }
        if self.accepted_at.len() >= MAX_TRACKER_DIAGNOSTICS_PER_WINDOW as usize {
            return false;
        }
        self.accepted_at.push_back(now);
        true
    }
}

fn sanitize_recording_error_message(message: &str) -> String {
    let safe = message
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_RECORDING_ERROR_MESSAGE_CHARS)
        .collect::<String>();
    if safe.is_empty() {
        "Unknown recording bridge error".to_string()
    } else {
        safe
    }
}

struct MacroState {
    file: PathBuf,
    log_file: PathBuf,
    performance_file: PathBuf,
    tracker_diagnostics_file: PathBuf,
    tracker_sessions_file: PathBuf,
    tracker_context_file: PathBuf,
    tracker_legacy_file: PathBuf,
    tracker_data_lock: Mutex<()>,
    tracker_dedupe_keys: Mutex<HashSet<String>>,
    tracker_diagnostic_rate_limit: Mutex<TrackerDiagnosticRateLimit>,
    cdp_port: u16,
    mode: AtomicU8,
    playback_generation: AtomicU64,
    bridge_ready: AtomicBool,
    record_started: Mutex<Option<Instant>>,
    clicks: Mutex<Vec<MacroClick>>,
    loop_enabled: AtomicBool,
}

fn can_load_macro(mode: u8) -> bool {
    mode == MODE_IDLE
}

// Serialize macro-buffer replacement with recording startup. Keep the lock
// order consistent with record_click: recording timer first, click buffer second.
fn begin_recording_if_idle(
    mode: &AtomicU8,
    record_started: &Mutex<Option<Instant>>,
    clicks: &Mutex<Vec<MacroClick>>,
) -> Result<bool, String> {
    let mut started = record_started
        .lock()
        .map_err(|_| "Recording timer is unavailable".to_string())?;
    let mut clicks = clicks
        .lock()
        .map_err(|_| "Macro data is unavailable".to_string())?;

    if mode
        .compare_exchange(
            MODE_IDLE,
            MODE_RECORDING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Ok(false);
    }

    clicks.clear();
    *started = Some(Instant::now());
    Ok(true)
}

// Hold the click-buffer lock until playback owns the mode. This prevents a
// concurrent load from replacing the buffer between snapshotting and playback.
fn begin_playback_if_idle(
    mode: &AtomicU8,
    clicks: &Mutex<Vec<MacroClick>>,
) -> Result<Option<Vec<MacroClick>>, String> {
    let clicks = clicks
        .lock()
        .map_err(|_| "Macro data is unavailable".to_string())?;
    if mode.load(Ordering::SeqCst) != MODE_IDLE {
        return Ok(None);
    }

    let snapshot = clicks.clone();
    if snapshot.is_empty() {
        return Ok(Some(snapshot));
    }

    if mode
        .compare_exchange(MODE_IDLE, MODE_PLAYING, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Ok(None);
    }
    Ok(Some(snapshot))
}

// Move every history source out of the live paths before deleting any file.
// This makes a partial staging failure reversible and prevents legacy migration
// from silently bringing old records back after an incomplete clear operation.
fn quarantine_history_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    quarantine_history_files_with(paths, |source, destination| fs::rename(source, destination))
}

fn quarantine_history_files_with<F>(
    paths: &[PathBuf],
    mut rename_file: F,
) -> Result<Vec<PathBuf>, String>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    static CLEAR_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();

    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                let rollback_errors = restore_quarantined_history_files(&staged);
                return Err(history_clear_stage_error(&error.to_string(), &rollback_errors));
            }
        }

        let Some(parent) = path.parent() else {
            let rollback_errors = restore_quarantined_history_files(&staged);
            return Err(history_clear_stage_error(
                "A Run History path has no parent directory",
                &rollback_errors,
            ));
        };
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            let rollback_errors = restore_quarantined_history_files(&staged);
            return Err(history_clear_stage_error(
                "A Run History filename could not be represented safely",
                &rollback_errors,
            ));
        };

        let mut quarantine = None;
        for _ in 0..10 {
            let sequence = CLEAR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                "{file_name}.clear-{}-{sequence}",
                std::process::id()
            ));
            match fs::symlink_metadata(&candidate) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    quarantine = Some(candidate);
                    break;
                }
                Ok(_) => continue,
                Err(error) => {
                    let rollback_errors = restore_quarantined_history_files(&staged);
                    return Err(history_clear_stage_error(&error.to_string(), &rollback_errors));
                }
            }
        }
        let Some(quarantine) = quarantine else {
            let rollback_errors = restore_quarantined_history_files(&staged);
            return Err(history_clear_stage_error(
                "Could not reserve a unique Run History quarantine filename",
                &rollback_errors,
            ));
        };

        if let Err(error) = rename_file(path, &quarantine) {
            let rollback_errors = restore_quarantined_history_files(&staged);
            return Err(history_clear_stage_error(&error.to_string(), &rollback_errors));
        }
        staged.push((path.clone(), quarantine));
    }

    Ok(staged.into_iter().map(|(_, quarantine)| quarantine).collect())
}

fn restore_quarantined_history_files(staged: &[(PathBuf, PathBuf)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (original, quarantine) in staged.iter().rev() {
        if let Err(error) = fs::rename(quarantine, original) {
            errors.push(format!(
                "{} could not be restored from {}: {error}",
                original.display(),
                quarantine.display()
            ));
        }
    }
    errors
}

fn history_clear_stage_error(reason: &str, rollback_errors: &[String]) -> String {
    if rollback_errors.is_empty() {
        format!("Run History was not cleared; original files were restored: {reason}")
    } else {
        format!(
            "Run History clear staging failed: {reason}. Some files may remain in quarantine and need recovery: {}",
            rollback_errors.join("; ")
        )
    }
}

fn parse_macro_file(bytes: &[u8]) -> Result<MacroFile, String> {
    let data: MacroFile = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    if data.format != "BFRMACRO2" && data.format != "BFRMACRO3" {
        return Err("This macro file format is not supported".to_string());
    }
    validate_macro_clicks(&data.clicks)?;
    Ok(data)
}

impl MacroState {
    fn save_macro_snapshot(&self, clicks: &[MacroClick]) -> Result<usize, String> {
        if clicks.is_empty() {
            return Ok(0);
        }
        validate_macro_clicks(clicks)?;
        let count = clicks.len();
        let data = MacroFile {
            format: "BFRMACRO3".to_string(),
            clicks: clicks.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&data).map_err(|e| e.to_string())?;
        write_file_atomically(&self.file, &bytes)?;
        Ok(count)
    }

    // Restore the last saved macro into memory while new recording/playback/load
    // operations are still blocked by MODE_STOPPING.
    fn restore_saved_macro_during_stopping(&self) -> Result<usize, String> {
        if !self.file.exists() {
            return Ok(0);
        }
        let metadata = fs::metadata(&self.file).map_err(|error| error.to_string())?;
        if metadata.len() > MAX_MACRO_FILE_BYTES {
            return Err("The saved macro is too large to restore safely".to_string());
        }
        let bytes = fs::read(&self.file).map_err(|error| error.to_string())?;
        let data = parse_macro_file(&bytes)?;
        let count = data.clicks.len();
        let mut clicks = self
            .clicks
            .lock()
            .map_err(|_| "Macro data is unavailable".to_string())?;
        if self.mode.load(Ordering::SeqCst) != MODE_STOPPING {
            return Err(
                "Recording state changed before the previous macro could be restored".to_string(),
            );
        }
        *clicks = data.clicks;
        Ok(count)
    }

    fn load_macro(&self) -> Result<usize, String> {
        if !can_load_macro(self.mode.load(Ordering::SeqCst)) {
            return Err("Stop recording or playback before loading a saved macro".to_string());
        }
        let metadata = fs::metadata(&self.file).map_err(|e| e.to_string())?;
        if metadata.len() > MAX_MACRO_FILE_BYTES {
            return Err("The macro file is too large to load safely (16 MB maximum)".to_string());
        }
        let bytes = fs::read(&self.file).map_err(|e| e.to_string())?;
        let data = parse_macro_file(&bytes)?;
        let count = data.clicks.len();
        let mut clicks = self
            .clicks
            .lock()
            .map_err(|_| "Macro data is unavailable".to_string())?;
        // Recheck while holding the click buffer lock so a recording cannot begin
        // between the initial mode check and replacing the in-memory macro.
        if !can_load_macro(self.mode.load(Ordering::SeqCst)) {
            return Err("Stop recording or playback before loading a saved macro".to_string());
        }
        *clicks = data.clicks;
        Ok(count)
    }

    fn log(&self, message: &str) {
        if let Some(parent) = self.log_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let line = format!("{}  {}\n", chrono_like_timestamp(), message);
        use std::io::Write as _;
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn performance_event(&self, event: &str, details: Value) {
        let record = json!({
            "schema_version": 1,
            "record_type": "event",
            "timestamp_unix_ms": unix_timestamp_ms(),
            "event": event,
            "activity": activity_name(self.mode.load(Ordering::SeqCst)),
            "details": details
        });
        if let Err(error) = append_performance_record(&self.performance_file, &record) {
            self.log(&format!("Performance log write failed: {error}"));
        }
    }

    fn tracker_diagnostic_event(&self, event: &str, details: Value) -> Result<(), String> {
        let safe_event = match event {
            // Keep this list limited to events emitted by INJECTION_SCRIPT.
            "observer_started"
            | "observer_error"
            | "dungeon_context_seen"
            | "begin_quest_click_seen"
            | "repeat_action_click_seen"
            | "reward_persisted"
            | "reward_persist_failed"
            | "observer_status" => event,
            _ => return Err("Unsupported tracker diagnostic event".to_string()),
        };
        let allowed = self
            .tracker_diagnostic_rate_limit
            .lock()
            .map_err(|_| "Tracker diagnostic rate limiter is unavailable".to_string())?
            .allow();
        if !allowed {
            return Ok(());
        }
        let safe_details = match safe_event {
            "observer_started" => json!({
                "page_host": details.get("page_host").and_then(Value::as_str).unwrap_or("").chars().take(120).collect::<String>(),
                "page_path": details.get("page_path").and_then(Value::as_str).unwrap_or("").chars().take(200).collect::<String>(),
                "frame_is_top": details.get("frame_is_top").and_then(Value::as_bool).unwrap_or(true)
            }),
            "observer_status" => json!({
                "component": details.get("component").and_then(Value::as_str).unwrap_or("unknown").chars().take(40).collect::<String>(),
                "status": details.get("status").and_then(Value::as_str).unwrap_or("unknown").chars().take(30).collect::<String>(),
                "page_path": details.get("page_path").and_then(Value::as_str).unwrap_or("").chars().take(200).collect::<String>(),
                "diagnostic_sequence": details.get("diagnostic_sequence").and_then(Value::as_u64).unwrap_or(0).min(100000000),
                "diagnostic_failures": details.get("diagnostic_failures").and_then(Value::as_u64).unwrap_or(0).min(100000000),
                "fetch": details.get("fetch").and_then(Value::as_bool).unwrap_or(false),
                "xhr": details.get("xhr").and_then(Value::as_bool).unwrap_or(false),
                "websocket": details.get("websocket").and_then(Value::as_bool).unwrap_or(false),
                "dom": details.get("dom").and_then(Value::as_bool).unwrap_or(false),
                "performance_resource": details.get("performance_resource").and_then(Value::as_bool).unwrap_or(false)
            }),
            "begin_quest_click_seen"
            | "repeat_action_click_seen" => json!({
                "dungeon_id": details.get("dungeon_id").and_then(Value::as_str).unwrap_or("").chars().take(40).collect::<String>(),
                "route": details.get("route").and_then(Value::as_str).unwrap_or("").chars().take(200).collect::<String>(),
                "action_kind": details.get("action_kind").and_then(Value::as_str).filter(|v| ["batch", "battles", "generic"].contains(v)).unwrap_or("begin_quest"),
                "element_tag": details.get("element_tag").and_then(Value::as_str).unwrap_or("").chars().take(20).collect::<String>(),
                "frame_is_top": details.get("frame_is_top").and_then(Value::as_bool).unwrap_or(true),
                "started_at_unix_ms": details.get("started_at_unix_ms").and_then(Value::as_u64).unwrap_or(0)
            }),
            "dungeon_context_seen" => {
                let host = details.get("host").and_then(Value::as_str).unwrap_or("");
                if host != "rebornbf.com" {
                    return Ok(());
                }
                let route = details.get("route").and_then(Value::as_str).unwrap_or("");
                if !route.starts_with("/dungeons/") && !route.starts_with("/start/") {
                    return Ok(());
                }
                let safe_id = details
                    .get("dungeon_id")
                    .and_then(Value::as_str)
                    .filter(|v| {
                        !v.is_empty()
                            && v.len() <= 40
                            && v.chars()
                                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
                    })
                    .unwrap_or("");
                let name = details
                    .get("dungeon_name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                json!({
                    "host": host,
                    "route": route.chars().take(200).collect::<String>(),
                    "dungeon_id": safe_id,
                    "dungeon_name": name.chars().take(100).filter(|c| !c.is_control()).collect::<String>(),
                    "source": details.get("source").and_then(Value::as_str).unwrap_or("route").chars().take(40).collect::<String>()
                })
            }
            "reward_persisted" => json!({
                "dungeon_id": details.get("dungeon_id").and_then(Value::as_str).unwrap_or("").chars().take(40).collect::<String>(),
                "settled_runs": details.get("settled_runs").and_then(Value::as_u64).unwrap_or(0).min(100000)
            }),
            _ => json!({
                "stage": details.get("stage").and_then(Value::as_str).unwrap_or("unknown").chars().take(80).collect::<String>(),
                "message": details.get("message").and_then(Value::as_str).unwrap_or("unknown").chars().take(180).collect::<String>()
            }),
        };
        let record = json!({
            "schema_version": 1,
            "record_type": "tracker_discovery",
            "timestamp_unix_ms": unix_timestamp_ms(),
            "event": safe_event,
            "details": safe_details
        });
        append_performance_record(&self.tracker_diagnostics_file, &record)
    }

    fn tracker_dungeon_context(&self, details: Value) -> Result<(), String> {
        let host = details.get("host").and_then(Value::as_str).unwrap_or("");
        let route = details.get("route").and_then(Value::as_str).unwrap_or("");
        if host != "rebornbf.com"
            || !route.starts_with('/')
            || route.starts_with("//")
            || route.len() > 200
            || route.contains('?')
            || route.contains('#')
        {
            return Err("Rejected invalid dungeon context from the game page".to_string());
        }
        let safe_id = details
            .get("dungeon_id")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 40
                    && value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            })
            .map(str::to_string);
        let supplied_name = details
            .get("dungeon_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| {
                !value.is_empty() && value.len() <= 100 && !value.chars().any(char::is_control)
            });
        let name = supplied_name.or(match safe_id.as_deref() {
            Some("100501") => Some("Gold Valley Lv. 2"),
            Some("100502") => Some("Gold Valley Lv. 3"),
            _ => None,
        });
        let record = json!({
            "schema_version": 1,
            "record_type": "dungeon_context",
            "timestamp_unix_ms": unix_timestamp_ms(),
            "dungeon_route": route,
            "dungeon_id": safe_id,
            "dungeon_name": name,
            "dungeon_started_at_unix_ms": details.get("dungeon_started_at_unix_ms").and_then(Value::as_u64).filter(|value| *value > 0),
            "dungeon_start_source": details.get("dungeon_start_source").and_then(Value::as_str).filter(|v| ["begin_quest_click", "repeat_action_click", "api_fallback"].contains(v)),
            "run_active": details.get("run_active").and_then(Value::as_bool).unwrap_or(false),
            "run_type": details.get("run_type").and_then(Value::as_str).filter(|v| ["single", "repeat", "repeat_batch"].contains(v)).unwrap_or("single")
        });
        let _history_guard = self.tracker_data_lock.lock().map_err(|_| {
            "Tracker history is unavailable because its lock was poisoned".to_string()
        })?;
        append_performance_record(&self.tracker_context_file, &record)
    }

    fn tracker_reward_event(&self, details: Value) -> Result<bool, String> {
        let host = details.get("host").and_then(Value::as_str).unwrap_or("");
        let path = details.get("path").and_then(Value::as_str).unwrap_or("");
        if host != "rebornbf.com"
            || !(path == "/api/battles/repeat" || path == "/api/battles/finish")
        {
            return Ok(false);
        }
        let _history_guard = self.tracker_data_lock.lock().map_err(|_| {
            "Tracker history is unavailable because its lock was poisoned".to_string()
        })?;

        let safe_id = |value: Option<&Value>, max: usize| -> Option<String> {
            let raw = value?.as_str()?;
            if raw.is_empty()
                || raw.len() > max
                || !raw
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
            {
                return None;
            }
            Some(raw.to_string())
        };
        let safe_number = |object: &Value, key: &str| -> Option<u64> {
            object
                .get(key)?
                .as_u64()
                .filter(|n| *n <= 9_000_000_000_000)
        };
        let settlement = details.get("settlement").cloned().unwrap_or(Value::Null);
        let totals = details.get("totals").cloned().unwrap_or(Value::Null);
        let numeric_keys = [
            "exp",
            "zel",
            "gems",
            "runs",
            "wins",
            "honor",
            "karma",
            "level",
            "settled_runs",
            "auto_sell_zel",
        ];
        let mut settlement_safe = serde_json::Map::new();
        let mut totals_safe = serde_json::Map::new();
        for key in numeric_keys {
            if let Some(value) = safe_number(&settlement, key) {
                settlement_safe.insert(key.into(), json!(value));
            }
            if let Some(value) = safe_number(&totals, key) {
                totals_safe.insert(key.into(), json!(value));
            }
        }
        let sanitize_items = |source: Option<&Value>| {
            let mut safe = serde_json::Map::new();
            if let Some(items) = source.and_then(Value::as_object) {
                for (key, value) in items.iter().take(200) {
                    let valid_key = !key.is_empty()
                        && key.len() <= 80
                        && key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
                    if !valid_key {
                        continue;
                    }
                    if let Some(amount) = value.as_u64().filter(|n| *n <= 9_000_000_000_000) {
                        safe.insert(key.clone(), json!(amount));
                    }
                }
            }
            Value::Object(safe)
        };
        let settlement_items = sanitize_items(details.get("items"));
        let totals_items = sanitize_items(details.get("totals_items"));
        let route = details
            .get("dungeon_route")
            .and_then(Value::as_str)
            .unwrap_or("");
        let route = if (route.starts_with("/dungeons/") || route.starts_with("/start/"))
            && route.len() <= 200
            && !route.contains('?')
            && !route.contains('#')
        {
            route
        } else {
            ""
        };
        let dungeon_id = safe_id(details.get("dungeon_id"), 40);
        let supplied_name = details
            .get("dungeon_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 100 && !s.chars().any(|c| c.is_control()));
        // Only use a name explicitly supplied by the current game page.
        // No ID-to-name guesses: unknown quests are displayed by run ID.
        let dungeon_name = supplied_name.or(match dungeon_id.as_deref() {
            Some("100501") => Some("Gold Valley Lv. 2"),
            Some("100502") => Some("Gold Valley Lv. 3"),
            _ => None,
        });
        let batch_id = safe_id(details.get("batch_id"), 100).unwrap_or_default();
        let authoritative_run_type = if path == "/api/battles/repeat" {
            if batch_id.is_empty() {
                "repeat"
            } else {
                "repeat_batch"
            }
        } else {
            "single"
        };
        let mut record = json!({
            "schema_version": 1,
            "record_type": "battle_reward",
            "timestamp_unix_ms": unix_timestamp_ms(),
            "path": path,
            "dungeon_route": route,
            "dungeon_started_at_unix_ms": details.get("dungeon_started_at_unix_ms").and_then(Value::as_u64).filter(|v| *v > 0),
            "dungeon_start_source": details.get("dungeon_start_source").and_then(Value::as_str).filter(|v| ["begin_quest_click", "repeat_action_click", "api_fallback"].contains(v)),
            "dungeon_id": dungeon_id.clone(),
            "dungeon_name": dungeon_name,
            "batch_id": if batch_id.is_empty() { Value::Null } else { json!(batch_id) },
            "run_type": authoritative_run_type,
            "settlement": settlement_safe,
            "totals": totals_safe,
            "settlement_finished": settlement.get("finished").and_then(Value::as_bool).unwrap_or(false),
            "totals_finished": totals.get("finished").and_then(Value::as_bool).unwrap_or(false),
            "quest_finished": details.get("quest_finished").and_then(Value::as_bool).unwrap_or(path == "/api/battles/finish"),
            "items": settlement_items,
            "totals_items": totals_items
        });
        // Canonical run accounting is shared by persistence, deduplication and logs.
        // /finish is exactly one completed run. /repeat reports a cumulative batch
        // snapshot; prefer explicit settled_runs, then runs/wins in settlement/totals.
        let batch_id = record
            .get("batch_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let finish_path = path == "/api/battles/finish";
        let run_count = if finish_path {
            1
        } else {
            repeat_cumulative_run_count(&settlement, &totals).unwrap_or(0)
        };
        if run_count == 0 {
            self.log("Tracker reward ignored: repeat response did not contain a valid cumulative run count.");
            return Ok(false);
        }
        record["run_count"] = json!(run_count);
        record["run_count_source"] = json!(if finish_path {
            "single_completion"
        } else {
            "repeat_cumulative"
        });

        // Repeat snapshots are deduplicated by batch ID + cumulative run count.
        // Later snapshots with a higher count are retained as updates. Single-run
        // completions are deduplicated by dungeon route + entry timestamp.
        let finish_start = record
            .get("dungeon_started_at_unix_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let finish_route = record
            .get("dungeon_route")
            .and_then(Value::as_str)
            .unwrap_or("");
        let dedupe_key = if !batch_id.is_empty() && !finish_path {
            Some(format!("{batch_id}:{run_count}"))
        } else if finish_path && finish_start > 0 && !finish_route.is_empty() {
            Some(format!("finish:{finish_route}:{finish_start}"))
        } else {
            None
        };
        if let Some(key) = dedupe_key.as_deref() {
            let keys = self
                .tracker_dedupe_keys
                .lock()
                .map_err(|_| "Tracker deduplication cache is unavailable".to_string())?;
            if keys.contains(key) {
                self.log(&format!("Tracker duplicate ignored (key {key})."));
                return Ok(false);
            }
        }
        if let Some(key) = dedupe_key {
            record["dedupe_key"] = json!(key);
        }
        match append_history_record(&self.tracker_sessions_file, &record) {
            Ok(()) => {
                if let Some(key) = record.get("dedupe_key").and_then(Value::as_str) {
                    if let Ok(mut keys) = self.tracker_dedupe_keys.lock() {
                        keys.insert(key.to_string());
                    }
                }
                self.log(&format!(
                    "Tracker reward saved (dungeon {}, path {}, batch {}, runs {}).",
                    dungeon_id.as_deref().unwrap_or("unknown"),
                    path,
                    batch_id,
                    run_count
                ));
                Ok(true)
            }
            Err(error) => {
                self.log(&format!("Tracker reward save failed: {error}"));
                Err(error)
            }
        }
    }

    fn tracker_read_data_page(&self, offset: usize, anchor_total: usize, requested_limit: usize) -> Result<TrackerHistoryPage, String> {
        let _history_guard = self.tracker_data_lock.lock().map_err(|_| {
            "Tracker history is unavailable because its lock was poisoned".to_string()
        })?;
        let backup_file = self.tracker_sessions_file.with_extension("jsonl.1");
        let (page, malformed_lines) = read_jsonl_page(
            &[backup_file.as_path(), self.tracker_sessions_file.as_path()],
            offset,
            anchor_total,
            requested_limit,
        )?;
        if malformed_lines > 0 {
            self.log(&format!("Run History skipped {malformed_lines} malformed or oversized JSONL record(s); valid records remain available."));
        }
        Ok(page)
    }

    fn tracker_clear_history(&self) -> Result<(), String> {
        let _history_guard = self.tracker_data_lock.lock().map_err(|_| {
            "Tracker history is unavailable because its lock was poisoned".to_string()
        })?;

        // Stage every source under a unique sibling name before deleting any of
        // them. If a rename fails, previously staged files are restored in reverse
        // order so the original history remains readable and migration-safe.
        let migrated_legacy_file = self.tracker_legacy_file.with_extension("jsonl.migrated");
        let backup_file = self.tracker_sessions_file.with_extension("jsonl.1");
        let staged_files = quarantine_history_files(&[
            self.tracker_sessions_file.clone(),
            backup_file,
            self.tracker_legacy_file.clone(),
            migrated_legacy_file,
        ])?;

        // All active/recovery paths are now detached, so no old records can be
        // re-imported. A cleanup failure leaves a clearly named quarantine file
        // rather than restoring data into the live history; report it in the log.
        let mut undeleted = Vec::new();
        for quarantined in staged_files {
            if let Err(error) = fs::remove_file(&quarantined) {
                undeleted.push(format!("{} ({error})", quarantined.display()));
            }
        }
        if let Ok(mut keys) = self.tracker_dedupe_keys.lock() {
            keys.clear();
        }
        if undeleted.is_empty() {
            self.log("Run History cleared by the user.");
        } else {
            self.log(&format!(
                "Run History was detached successfully, but quarantined files could not be permanently deleted: {}",
                undeleted.join("; ")
            ));
        }
        Ok(())
    }
}

// A compact local timestamp without another dependency.
fn chrono_like_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("unix:{secs}")
}

fn unix_timestamp_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn activity_name(mode: u8) -> &'static str {
    match mode {
        MODE_RECORDING => "recording",
        MODE_PLAYING => "playback",
        MODE_STOPPING => "stopping",
        _ => "idle",
    }
}

// Serialize each JSONL record into one buffer before appending. Diagnostics may
// rotate; user-facing Run History must never rotate away records automatically.
// Ensure a new JSONL record starts on its own line without changing any
// existing bytes. A malformed/incomplete final record remains available for
// diagnosis; the separator only prevents it from swallowing the next record.
fn ensure_jsonl_line_boundary(file: &mut fs::File) -> std::io::Result<()> {
    let length = file.seek(SeekFrom::End(0))?;
    if length == 0 {
        return Ok(());
    }

    file.seek(SeekFrom::End(-1))?;
    let mut last_byte = [0u8; 1];
    file.read_exact(&mut last_byte)?;
    file.seek(SeekFrom::End(0))?;

    if last_byte[0] != b'\n' {
        file.write_all(b"\n")?;
    }
    Ok(())
}

fn append_jsonl_record(path: &Path, record: &Value, rotate: bool) -> Result<(), String> {
    const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if rotate
        && fs::metadata(path)
            .map(|metadata| metadata.len() >= MAX_LOG_BYTES)
            .unwrap_or(false)
    {
        let previous = path.with_extension("jsonl.1");
        let _ = fs::remove_file(&previous);
        fs::rename(path, previous).map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    ensure_jsonl_line_boundary(&mut file).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())
}

fn append_performance_record(path: &Path, record: &Value) -> Result<(), String> {
    append_jsonl_record(path, record, true)
}

fn append_history_record(path: &Path, record: &Value) -> Result<(), String> {
    append_jsonl_record(path, record, false)
}

const MAX_JSONL_LINE_BYTES: usize = 1024 * 1024;

// Read one JSONL line without allowing a corrupt or hostile line to allocate
// unbounded memory. Oversized lines are drained through the next newline so
// subsequent valid history records remain readable.
fn read_bounded_jsonl_line<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut oversized = false;
    let mut read_any = false;

    loop {
        let (consumed, has_newline) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(if read_any { Some(!oversized) } else { None });
            }

            let newline_index = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline_index.map_or(available.len(), |index| index + 1);
            let content_length = newline_index.unwrap_or(consumed);

            if !oversized {
                let remaining = MAX_JSONL_LINE_BYTES.saturating_sub(line.len());
                let to_copy = content_length.min(remaining);
                line.extend_from_slice(&available[..to_copy]);
                if to_copy < content_length {
                    oversized = true;
                }
            }

            (consumed, newline_index.is_some())
        };

        reader.consume(consumed);
        read_any = true;
        if has_newline {
            return Ok(Some(!oversized));
        }
    }
}

const MAX_HISTORY_PAGE_SIZE: usize = 200;

#[derive(Serialize)]
struct TrackerHistoryPage {
    records: Vec<Value>,
    total_records: usize,
    next_offset: usize,
    has_more: bool,
}

fn scan_jsonl_records<F>(path: &Path, mut visit: F) -> Result<usize, String>
where
    F: FnMut(Value),
{
    if !path.exists() { return Ok(0); }
    let file = fs::File::open(path)
        .map_err(|error| format!("Could not open tracker data file: {error}"))?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    let mut malformed_lines = 0usize;
    while let Some(line_is_within_limit) = read_bounded_jsonl_line(&mut reader, &mut line)
        .map_err(|error| format!("Could not read tracker data: {error}"))?
    {
        if !line_is_within_limit { malformed_lines += 1; continue; }
        while line.last().is_some_and(|last| *last == b'\r') { line.pop(); }
        if line.iter().all(u8::is_ascii_whitespace) { continue; }
        match serde_json::from_slice::<Value>(&line) {
            Ok(record) => visit(record),
            Err(_) => malformed_lines += 1,
        }
    }
    Ok(malformed_lines)
}

fn read_jsonl_records(path: &Path, records: &mut Vec<Value>, malformed_lines: &mut usize) -> Result<(), String> {
    *malformed_lines += scan_jsonl_records(path, |record| records.push(record))?;
    Ok(())
}

fn read_jsonl_page(
    paths: &[&Path],
    offset: usize,
    anchor_total: usize,
    requested_limit: usize,
) -> Result<(TrackerHistoryPage, usize), String> {
    let limit = requested_limit.clamp(1, MAX_HISTORY_PAGE_SIZE);
    let mut total_records = 0usize;
    let mut malformed_lines = 0usize;
    for path in paths {
        malformed_lines += scan_jsonl_records(path, |_| {
            total_records = total_records.saturating_add(1);
        })?;
    }

    // Anchor offsets to the record count from the previous page. New appends
    // shift the physical tail, so account for them without skipping older rows.
    let newly_appended = if anchor_total == 0 { 0 } else { total_records.saturating_sub(anchor_total) };
    let effective_offset = offset.saturating_add(newly_appended);
    let end = total_records.saturating_sub(effective_offset);
    let start = end.saturating_sub(limit);
    let mut records = Vec::with_capacity(end.saturating_sub(start));
    let mut record_index = 0usize;
    for path in paths {
        let _ = scan_jsonl_records(path, |record| {
            if record_index >= start && record_index < end {
                records.push(record);
            }
            record_index = record_index.saturating_add(1);
        })?;
    }

    let next_offset = effective_offset.saturating_add(records.len());
    Ok((
        TrackerHistoryPage {
            records,
            total_records,
            next_offset,
            has_more: start > 0,
        },
        malformed_lines,
    ))
}

fn load_history_dedupe_keys(backup: &Path, active: &Path) -> Result<HashSet<String>, String> {
    let mut records = Vec::new();
    let mut malformed_lines = 0usize;
    read_jsonl_records(backup, &mut records, &mut malformed_lines)?;
    read_jsonl_records(active, &mut records, &mut malformed_lines)?;
    Ok(records
        .into_iter()
        .filter_map(|record| {
            record
                .get("dedupe_key")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect())
}

fn repeat_cumulative_run_count(settlement: &Value, totals: &Value) -> Option<u64> {
    let valid = |object: &Value, key: &str| {
        object
            .get(key)
            .and_then(Value::as_u64)
            .filter(|count| *count > 0 && *count <= 1_000_000)
    };
    // Observed game responses show settlement.runs is the current chunk (e.g. 10),
    // while settlement.settled_runs and totals.* are cumulative batch totals.
    valid(settlement, "settled_runs")
        .or_else(|| valid(totals, "settled_runs"))
        .or_else(|| valid(totals, "runs"))
        .or_else(|| valid(totals, "wins"))
}

fn validate_macro_clicks(clicks: &[MacroClick]) -> Result<(), String> {
    if clicks.len() > MAX_MACRO_CLICKS {
        return Err(format!(
            "The macro contains too many clicks (maximum {MAX_MACRO_CLICKS})"
        ));
    }
    if clicks.iter().any(|click| {
        !click.x.is_finite()
            || !click.y.is_finite()
            || click.x < 0.0
            || click.x > 1.0
            || click.y < 0.0
            || click.y > 1.0
    }) {
        return Err("The macro contains invalid click coordinates".to_string());
    }
    if clicks
        .iter()
        .any(|click| click.elapsed_ms > MAX_MACRO_DURATION_MS)
    {
        return Err("The macro contains a delay longer than the 24-hour safety limit".to_string());
    }
    if clicks
        .windows(2)
        .any(|pair| pair[1].elapsed_ms < pair[0].elapsed_ms)
    {
        return Err("The macro contains click timestamps that are out of order".to_string());
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Default)]
struct ProcessMetrics {
    working_set_bytes: Option<u64>,
    private_bytes: Option<u64>,
    cpu_percent: Option<f64>,
    system_memory_load_percent: Option<u32>,
    system_total_memory_bytes: Option<u64>,
    system_available_memory_bytes: Option<u64>,
    process_details: Vec<Value>,
}

#[cfg(windows)]
fn collect_process_metrics(previous_cpu: &mut Option<(u64, Instant)>) -> ProcessMetrics {
    use std::{ffi::c_void, mem::size_of};

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    impl FileTime {
        fn ticks(&self) -> u64 {
            ((self.high as u64) << 32) | self.low as u64
        }
    }
    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        base_priority: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcessId() -> u32;
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> i32;
    }
    #[link(name = "psapi")]
    extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            size: u32,
        ) -> i32;
    }

    const TH32CS_SNAPPROCESS: u32 = 0x00000002;
    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const PROCESS_VM_READ: u32 = 0x0010;
    const INVALID_HANDLE_VALUE: isize = -1;

    let mut result = ProcessMetrics::default();
    let root_pid = unsafe { GetCurrentProcessId() };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    let mut process_ids = vec![root_pid];
    let mut process_names = std::collections::HashMap::<u32, String>::new();
    if !snapshot.is_null() && snapshot as isize != INVALID_HANDLE_VALUE {
        let mut entries = Vec::<(u32, u32)>::new();
        let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
        entry.size = size_of::<ProcessEntry32W>() as u32;
        if unsafe { Process32FirstW(snapshot, &mut entry) } != 0 {
            loop {
                let end = entry
                    .exe_file
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(entry.exe_file.len());
                let name = String::from_utf16_lossy(&entry.exe_file[..end]);
                process_names.insert(entry.process_id, name.chars().take(80).collect());
                entries.push((entry.process_id, entry.parent_process_id));
                if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                    break;
                }
            }
        }
        unsafe {
            CloseHandle(snapshot);
        }
        // Include WebView2 renderer/GPU/utility descendants launched beneath this app.
        loop {
            let before = process_ids.len();
            for (pid, parent) in &entries {
                if process_ids.contains(parent) && !process_ids.contains(pid) {
                    process_ids.push(*pid);
                }
            }
            if process_ids.len() == before {
                break;
            }
        }
    }

    let mut working_set = 0_u64;
    let mut private_bytes = 0_u64;
    let mut cpu_ticks = 0_u64;
    let mut found_memory = false;
    let mut found_cpu = false;
    let mut process_details = Vec::new();
    for pid in process_ids {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
        if handle.is_null() {
            continue;
        }
        let mut memory: ProcessMemoryCountersEx = unsafe { std::mem::zeroed() };
        memory.cb = size_of::<ProcessMemoryCountersEx>() as u32;
        let memory_size = memory.cb;
        let mut process_working_set = None;
        let mut process_private = None;
        if unsafe { GetProcessMemoryInfo(handle, &mut memory, memory_size) } != 0 {
            working_set = working_set.saturating_add(memory.working_set_size as u64);
            private_bytes = private_bytes.saturating_add(memory.private_usage as u64);
            process_working_set = Some(memory.working_set_size as u64);
            process_private = Some(memory.private_usage as u64);
            found_memory = true;
        }
        let mut creation: FileTime = unsafe { std::mem::zeroed() };
        let mut exit: FileTime = unsafe { std::mem::zeroed() };
        let mut kernel: FileTime = unsafe { std::mem::zeroed() };
        let mut user: FileTime = unsafe { std::mem::zeroed() };
        let mut process_cpu_ticks = None;
        if unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) } != 0
        {
            let ticks = kernel.ticks().saturating_add(user.ticks());
            cpu_ticks = cpu_ticks.saturating_add(ticks);
            process_cpu_ticks = Some(ticks);
            found_cpu = true;
        }
        unsafe {
            CloseHandle(handle);
        }
        process_details.push(json!({
            "pid": pid,
            "image_name": process_names.get(&pid).cloned().unwrap_or_else(|| "unknown".to_string()),
            "working_set_mb": process_working_set.map(|v| v as f64 / 1_048_576.0),
            "private_mb": process_private.map(|v| v as f64 / 1_048_576.0),
            "cpu_time_ms": process_cpu_ticks.map(|v| v as f64 / 10_000.0)
        }));
    }
    if found_memory {
        result.working_set_bytes = Some(working_set);
        result.private_bytes = Some(private_bytes);
    }
    result.process_details = process_details;
    if found_cpu {
        let now = Instant::now();
        if let Some((old_ticks, old_time)) = previous_cpu.take() {
            let elapsed = now.duration_since(old_time).as_secs_f64();
            let delta = cpu_ticks.saturating_sub(old_ticks) as f64 / 10_000_000.0;
            let logical_cpus = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) as f64;
            if elapsed > 0.0 {
                result.cpu_percent =
                    Some((delta / elapsed / logical_cpus * 100.0).clamp(0.0, 100.0));
            }
        }
        *previous_cpu = Some((cpu_ticks, now));
    }

    let mut system = MemoryStatusEx {
        length: size_of::<MemoryStatusEx>() as u32,
        memory_load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page_file: 0,
        avail_page_file: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended_virtual: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut system) } != 0 {
        result.system_memory_load_percent = Some(system.memory_load);
        result.system_total_memory_bytes = Some(system.total_phys);
        result.system_available_memory_bytes = Some(system.avail_phys);
    }
    result
}

#[cfg(not(windows))]
fn collect_process_metrics(_previous_cpu: &mut Option<(u64, Instant)>) -> Value {
    json!({ "available": false, "reason": "Windows process metrics are only implemented on Windows." })
}

struct TrayMenuState {
    record: Arc<IconMenuItem<Wry>>,
    play: Arc<IconMenuItem<Wry>>,
    loop_item: Arc<IconMenuItem<Wry>>,
    pin: Arc<IconMenuItem<Wry>>,
}

#[derive(Deserialize)]
struct CdpTarget {
    #[serde(rename = "type")]
    target_type: String,
    url: Option<String>,
    #[serde(rename = "webSocketDebuggerUrl")]
    websocket_url: Option<String>,
}

#[cfg(windows)]
fn feedback_sound(error: bool) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBeep(kind: u32) -> i32;
    }
    const MB_ICONASTERISK: u32 = 0x00000040;
    const MB_ICONHAND: u32 = 0x00000010;
    unsafe {
        let _ = MessageBeep(if error { MB_ICONHAND } else { MB_ICONASTERISK });
    }
}

#[cfg(not(windows))]
fn feedback_sound(_error: bool) {}

fn notify(app: &AppHandle<Wry>, title: &str, body: &str, error: bool) {
    feedback_sound(error);
    let result = app.notification().builder().title(title).body(body).show();
    if let Err(notification_error) = result {
        if let Some(state) = app.try_state::<MacroState>() {
            state.log(&format!(
                "Notification failed: {notification_error}; intended message: {title} — {body}"
            ));
        }
    }
    if let Some(state) = app.try_state::<MacroState>() {
        state.log(&format!("{} — {}", title, body));
    }
}

fn refresh_tray_menu(app: &AppHandle<Wry>) {
    let Some(menu) = app.try_state::<TrayMenuState>() else {
        return;
    };
    let Some(state) = app.try_state::<MacroState>() else {
        return;
    };
    let mode = state.mode.load(Ordering::SeqCst);
    let has_macro = state
        .clicks
        .lock()
        .map(|clicks| !clicks.is_empty())
        .unwrap_or(false);

    let record_label = match mode {
        MODE_RECORDING => "Stop Recording",
        MODE_STOPPING => "Saving Recording...",
        _ => "Start Recording",
    };
    let _ = menu.record.set_text(record_label);
    // Recording controls remain clickable regardless of game-window focus or bridge timing.
    // start_recording reports a clear readiness error if the remote bridge is not connected.
    let _ = menu
        .record
        .set_enabled(mode != MODE_PLAYING && mode != MODE_STOPPING);

    let play_label = match mode {
        MODE_PLAYING => "Stop Playback",
        MODE_STOPPING => "Saving Recording...",
        _ => "Play Macro",
    };
    let _ = menu.play.set_text(play_label);
    let _ = menu.play.set_enabled(
        mode != MODE_RECORDING && mode != MODE_STOPPING && (mode == MODE_PLAYING || has_macro),
    );

    let loop_text = if state.loop_enabled.load(Ordering::SeqCst) {
        "Loop Continuously: On"
    } else {
        "Loop Continuously: Off"
    };
    let _ = menu.loop_item.set_text(loop_text);
    let pinned = app
        .try_state::<SettingsState>()
        .and_then(|settings| settings.settings.lock().ok().map(|value| value.pinned))
        .unwrap_or(false);
    let _ = menu
        .pin
        .set_text(if pinned { "Unpin Window" } else { "Pin Window" });
}

#[tauri::command]
fn recording_bridge_ready(app: AppHandle<Wry>, state: State<'_, MacroState>) {
    state.bridge_ready.store(true, Ordering::SeqCst);
    state.log("Game-page recording bridge connected.");
    refresh_tray_menu(&app);
}

#[tauri::command(async)]
fn tracker_diagnostic_event(
    event: String,
    details: Value,
    state: State<'_, MacroState>,
) -> Result<(), String> {
    state.tracker_diagnostic_event(&event, details)
}

#[tauri::command(async)]
fn tracker_dungeon_context(details: Value, state: State<'_, MacroState>) -> Result<(), String> {
    state.tracker_dungeon_context(details)
}

#[tauri::command(async)]
fn tracker_reward_event(details: Value, state: State<'_, MacroState>) -> Result<bool, String> {
    state.tracker_reward_event(details)
}

#[tauri::command(async)]
fn tracker_read_data_page(offset: usize, anchor_total: usize, limit: usize, state: State<'_, MacroState>) -> Result<TrackerHistoryPage, String> {
    state.tracker_read_data_page(offset, anchor_total, limit)
}

#[tauri::command(async)]
fn tracker_clear_history(state: State<'_, MacroState>) -> Result<(), String> {
    state.tracker_clear_history()
}

#[tauri::command]
fn tracker_get_pinned(settings: State<'_, SettingsState>) -> Result<bool, String> {
    settings
        .settings
        .lock()
        .map(|value| value.tracker_pinned)
        .map_err(|_| "Settings are unavailable".to_string())
}

#[tauri::command]
fn tracker_set_pinned(
    pinned: bool,
    app: AppHandle<Wry>,
    settings: State<'_, SettingsState>,
) -> Result<(), String> {
    let window = app
        .get_webview_window("tracker")
        .ok_or_else(|| "Run History window is not open".to_string())?;
    window
        .set_always_on_top(pinned)
        .map_err(|error| format!("Could not change tracker pin state: {error}"))?;
    {
        let mut value = settings
            .settings
            .lock()
            .map_err(|_| "Settings are unavailable".to_string())?;
        value.tracker_pinned = pinned;
    }
    settings.save()
}

#[tauri::command]
fn record_click(x: f64, y: f64, state: State<'_, MacroState>) -> Result<(), String> {
    if state.mode.load(Ordering::SeqCst) != MODE_RECORDING {
        return Ok(());
    }
    if !x.is_finite() || !y.is_finite() {
        return Err("The game page sent invalid click coordinates".to_string());
    }
    let elapsed_ms = state
        .record_started
        .lock()
        .map_err(|_| "Recording timer is unavailable".to_string())?
        .as_ref()
        .map(|instant| instant.elapsed().as_millis() as u64)
        .unwrap_or(0);
    if elapsed_ms > MAX_MACRO_DURATION_MS {
        return Err("Recording reached the 24-hour safety limit".to_string());
    }
    let mut clicks = state
        .clicks
        .lock()
        .map_err(|_| "Macro data is unavailable".to_string())?;
    // Recheck after acquiring the data lock. This prevents a late IPC call from
    // appending a click after stop_recording has taken its save snapshot.
    if state.mode.load(Ordering::SeqCst) == MODE_RECORDING {
        if clicks.len() >= MAX_MACRO_CLICKS {
            return Err(format!(
                "Recording reached the {MAX_MACRO_CLICKS}-click safety limit"
            ));
        }
        clicks.push(MacroClick {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
            elapsed_ms,
        });
    }
    Ok(())
}

// An immutable copy of the completed recording and its elapsed duration.
type RecordingSnapshot = (Vec<MacroClick>, Option<u128>);

// Enter a non-startable Stopping state while taking an immutable snapshot of the
// completed recording. Lock order matches begin_recording_if_idle.
fn begin_stopping_recording(
    mode: &AtomicU8,
    record_started: &Mutex<Option<Instant>>,
    clicks: &Mutex<Vec<MacroClick>>,
) -> Result<Option<RecordingSnapshot>, String> {
    let mut started = record_started
        .lock()
        .map_err(|_| "Recording timer is unavailable".to_string())?;
    let clicks = clicks
        .lock()
        .map_err(|_| "Macro data is unavailable".to_string())?;
    if mode
        .compare_exchange(
            MODE_RECORDING,
            MODE_STOPPING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Ok(None);
    }
    let duration = started.take().map(|instant| instant.elapsed().as_millis());
    Ok(Some((clicks.clone(), duration)))
}

fn finish_stopping_recording(mode: &AtomicU8) -> bool {
    mode.compare_exchange(MODE_STOPPING, MODE_IDLE, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[tauri::command]
fn report_recording_error(message: String, app: AppHandle<Wry>, state: State<'_, MacroState>) {
    let safe_message = sanitize_recording_error_message(&message);
    let snapshot = match begin_stopping_recording(&state.mode, &state.record_started, &state.clicks)
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return,
        Err(error) => {
            state.log(&format!(
                "Could not safely stop recording after capture error: {error}"
            ));
            return;
        }
    };
    let (clicks, recording_duration_ms) = snapshot;
    state.performance_event(
        "recording_error",
        json!({ "message": &safe_message, "recording_duration_ms": recording_duration_ms }),
    );
    state.log(&format!("Click capture IPC error: {safe_message}"));
    let recovery = match state.save_macro_snapshot(&clicks) {
        Ok(0) => {
            match state.restore_saved_macro_during_stopping() {
                Ok(_) => "No clicks had been captured; the previous saved macro was preserved.".to_string(),
                Err(error) => format!("No clicks were captured; the previous saved macro file was preserved, but could not be restored in memory: {error}"),
            }
        }
        Ok(count) => format!("Recovered and saved the {count} clicks captured before the error."),
        Err(error) => format!("Could not save the captured clicks: {error}"),
    };
    finish_stopping_recording(&state.mode);
    refresh_tray_menu(&app);
    notify(
        &app,
        "BFR Macro — Recording error",
        &format!(
            "Recording stopped because communication with the game failed: {safe_message}\n{recovery}"
        ),
        true,
    );
}

fn stop_recording(app: &AppHandle<Wry>) {
    let Some(state) = app.try_state::<MacroState>() else {
        return;
    };
    let snapshot = match begin_stopping_recording(&state.mode, &state.record_started, &state.clicks)
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return,
        Err(error) => {
            notify(app, "BFR Macro — Recording unavailable", &error, true);
            return;
        }
    };
    let (clicks, recording_duration_ms) = snapshot;
    state.performance_event(
        "recording_stopped",
        json!({
            "click_count": clicks.len(),
            "recording_duration_ms": recording_duration_ms
        }),
    );
    let result = match state.save_macro_snapshot(&clicks) {
        Ok(0) => {
            let restore_result = state.restore_saved_macro_during_stopping();
            match restore_result {
                Ok(_) => Ok(0),
                Err(error) => {
                    state.log(&format!("Could not restore the previous saved macro after an empty recording: {error}"));
                    Ok(0)
                }
            }
        }
        other => other,
    };
    finish_stopping_recording(&state.mode);
    refresh_tray_menu(app);
    match result {
        Ok(0) => notify(
            app,
            "BFR Macro — Recording stopped",
            "No clicks were captured, so the previous saved macro was kept.",
            false,
        ),
        Ok(count) => notify(
            app,
            "BFR Macro — Recording saved",
            &format!("Recording stopped. Automatically saved {count} clicks."),
            false,
        ),
        Err(error) => notify(app, "BFR Macro — Save failed", &error, true),
    }
}

fn start_recording(app: &AppHandle<Wry>) {
    let Some(state) = app.try_state::<MacroState>() else {
        return;
    };
    match state.mode.load(Ordering::SeqCst) {
        MODE_RECORDING => {
            stop_recording(app);
            return;
        }
        MODE_PLAYING => stop_playback(app),
        MODE_STOPPING => return,
        _ => {}
    }
    if !state.bridge_ready.load(Ordering::SeqCst) {
        notify(
            app,
            "BFR Macro — Game not ready",
            "The game click-capture bridge is not connected yet. Wait for the game page to finish loading, then try again.",
            true,
        );
        return;
    }
    match begin_recording_if_idle(&state.mode, &state.record_started, &state.clicks) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            notify(app, "BFR Macro — Recording unavailable", &error, true);
            return;
        }
    }
    state.performance_event("recording_started", json!({}));
    refresh_tray_menu(app);
    notify(
        app,
        "BFR Macro — Recording started",
        "Recording physical left-clicks inside the game. Select Stop Recording to automatically save.",
        false,
    );
}

fn stop_playback(app: &AppHandle<Wry>) {
    let Some(state) = app.try_state::<MacroState>() else {
        return;
    };
    if state.mode.load(Ordering::SeqCst) == MODE_PLAYING {
        // Invalidate the old worker before making the state available to a new one.
        state.playback_generation.fetch_add(1, Ordering::SeqCst);
        if state
            .mode
            .compare_exchange(MODE_PLAYING, MODE_IDLE, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            state.performance_event("playback_stopped", json!({}));
            refresh_tray_menu(app);
            notify(
                app,
                "BFR Macro — Playback stopped",
                "Macro playback has stopped.",
                false,
            );
        }
    }
}

fn playback_session_is_current(mode: u8, generation: u64, current_generation: u64) -> bool {
    mode == MODE_PLAYING && generation == current_generation
}

struct PlaybackSessionGuard {
    app: AppHandle<Wry>,
    generation: u64,
}

impl Drop for PlaybackSessionGuard {
    fn drop(&mut self) {
        if let Some(state) = self.app.try_state::<MacroState>() {
            if state.playback_generation.load(Ordering::SeqCst) == self.generation
                && state
                    .mode
                    .compare_exchange(MODE_PLAYING, MODE_IDLE, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                state.performance_event("playback_worker_exited_unexpectedly", json!({}));
                refresh_tray_menu(&self.app);
            }
        }
    }
}

fn playback_click_deadline(loop_started: Instant, elapsed_ms: u64) -> Option<Instant> {
    loop_started.checked_add(Duration::from_millis(elapsed_ms))
}

fn playback_click_position(click: &MacroClick, width: f64, height: f64) -> (f64, f64) {
    (
        (click.x * width).clamp(0.0, width - 1.0),
        (click.y * height).clamp(0.0, height - 1.0),
    )
}

fn playback_next_loop_deadline(loop_started: Instant, last_elapsed_ms: u64) -> Option<Instant> {
    playback_click_deadline(loop_started, last_elapsed_ms)?.checked_add(LOOP_GAP)
}

fn wait_while_playing_until(state: &MacroState, generation: u64, deadline: Instant) -> bool {
    loop {
        if !playback_session_is_current(
            state.mode.load(Ordering::SeqCst),
            generation,
            state.playback_generation.load(Ordering::SeqCst),
        ) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn is_game_page_url(value: &str) -> bool {
    Url::parse(value)
        .map(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("rebornbf.com")
                && url.port_or_known_default() == Some(443)
                && url.username().is_empty()
                && url.password().is_none()
        })
        .unwrap_or(false)
}

fn validate_cdp_websocket_url(value: &str, expected_port: u16) -> Result<(), String> {
    let url = Url::parse(value)
        .map_err(|error| format!("Invalid local DevTools WebSocket URL: {error}"))?;
    let valid = url.scheme() == "ws"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == Some(expected_port)
        && url.path().starts_with("/devtools/page/")
        && url.username().is_empty()
        && url.password().is_none();
    if valid {
        Ok(())
    } else {
        Err("The DevTools endpoint did not match the expected local WebView2 address".to_string())
    }
}

fn discover_cdp_target(port: u16) -> Result<String, String> {
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    let started = Instant::now();
    let mut last_error = String::from("WebView2 debugging endpoint is not ready yet");
    while started.elapsed() < CDP_DISCOVERY_TIMEOUT {
        match ureq::get(&endpoint).timeout(CDP_IO_TIMEOUT).call() {
            Ok(response) => match response.into_string() {
                Ok(body) => match serde_json::from_str::<Vec<CdpTarget>>(&body) {
                    Ok(targets) => {
                        if let Some(target) = targets.into_iter().find(|target| {
                            target.target_type == "page"
                                && target.url.as_deref().map(is_game_page_url).unwrap_or(false)
                        }) {
                            if let Some(url) = target.websocket_url {
                                validate_cdp_websocket_url(&url, port)?;
                                return Ok(url);
                            }
                        }
                    }
                    Err(error) => last_error = error.to_string(),
                },
                Err(error) => last_error = error.to_string(),
            },
            Err(error) => last_error = error.to_string(),
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "Could not find the Brave Frontier WebView2 page for DevTools playback: {last_error}"
    ))
}

fn cdp_call<S: Read + Write>(
    socket: &mut WebSocket<S>,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let id = *next_id;
    *next_id += 1;
    let request = json!({ "id": id, "method": method, "params": params });
    socket
        .send(Message::Text(request.to_string().into()))
        .map_err(|error| format!("DevTools send failed: {error}"))?;
    loop {
        let message = socket
            .read()
            .map_err(|error| format!("DevTools connection failed: {error}"))?;
        match message {
            Message::Text(text) => {
                let response: Value = serde_json::from_str(text.as_ref())
                    .map_err(|error| format!("Invalid DevTools response: {error}"))?;
                if response.get("id").and_then(Value::as_u64) == Some(id) {
                    if let Some(error) = response.get("error") {
                        return Err(format!("DevTools rejected {method}: {error}"));
                    }
                    return Ok(response.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            Message::Close(_) => return Err("WebView2 closed the DevTools connection".to_string()),
            Message::Ping(data) => {
                socket
                    .send(Message::Pong(data))
                    .map_err(|error| error.to_string())?;
            }
            _ => {}
        }
    }
}

fn viewport_size<S: Read + Write>(
    socket: &mut WebSocket<S>,
    next_id: &mut u64,
) -> Result<(f64, f64), String> {
    let result = cdp_call(
        socket,
        next_id,
        "Runtime.evaluate",
        json!({
            "expression": "({ width: window.innerWidth, height: window.innerHeight })",
            "returnByValue": true,
            "awaitPromise": true
        }),
    )?;
    let value = result
        .get("result")
        .and_then(|result| result.get("value"))
        .ok_or_else(|| "Could not read the game viewport dimensions".to_string())?;
    let width = value.get("width").and_then(Value::as_f64).unwrap_or(0.0);
    let height = value.get("height").and_then(Value::as_f64).unwrap_or(0.0);
    if width < 1.0 || height < 1.0 {
        return Err("The game viewport reported invalid dimensions".to_string());
    }
    Ok((width, height))
}

fn dispatch_click<S: Read + Write>(
    socket: &mut WebSocket<S>,
    next_id: &mut u64,
    x: f64,
    y: f64,
) -> Result<(), String> {
    cdp_call(
        socket,
        next_id,
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0, "pointerType": "mouse" }),
    )?;
    cdp_call(
        socket,
        next_id,
        "Input.dispatchMouseEvent",
        json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1, "pointerType": "mouse" }),
    )?;
    cdp_call(
        socket,
        next_id,
        "Input.dispatchMouseEvent",
        json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "buttons": 0, "clickCount": 1, "pointerType": "mouse" }),
    )?;
    Ok(())
}

fn playback_failed(app: &AppHandle<Wry>, generation: u64, message: &str) {
    if let Some(state) = app.try_state::<MacroState>() {
        if state.playback_generation.load(Ordering::SeqCst) == generation
            && state
                .mode
                .compare_exchange(MODE_PLAYING, MODE_IDLE, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            state.performance_event("playback_failed", json!({ "message": message }));
            refresh_tray_menu(app);
            notify(app, "BFR Macro — Playback failed", message, true);
        }
    }
}

fn start_playback(app: AppHandle<Wry>) {
    let Some(state) = app.try_state::<MacroState>() else {
        return;
    };
    if state.mode.load(Ordering::SeqCst) == MODE_PLAYING {
        stop_playback(&app);
        return;
    }
    if state.mode.load(Ordering::SeqCst) == MODE_RECORDING {
        stop_recording(&app);
    }
    let clicks = match begin_playback_if_idle(&state.mode, &state.clicks) {
        Ok(Some(clicks)) => clicks,
        Ok(None) => return,
        Err(error) => {
            notify(&app, "BFR Macro — Playback unavailable", &error, true);
            return;
        }
    };
    if clicks.is_empty() {
        notify(
            &app,
            "BFR Macro — No recording",
            "There is no saved macro to play. Record some clicks first.",
            true,
        );
        return;
    }
    let generation = state
        .playback_generation
        .fetch_add(1, Ordering::SeqCst)
        .wrapping_add(1);
    let port = state.cdp_port;
    state.performance_event("playback_started", json!({ "click_count": clicks.len(), "loop_enabled": state.loop_enabled.load(Ordering::SeqCst) }));
    refresh_tray_menu(&app);
    notify(
        &app,
        "BFR Macro — Connecting",
        "Connecting to the game input channel…",
        false,
    );

    thread::spawn(move || {
        let Some(state) = app.try_state::<MacroState>() else {
            return;
        };
        if !playback_session_is_current(
            state.mode.load(Ordering::SeqCst),
            generation,
            state.playback_generation.load(Ordering::SeqCst),
        ) {
            return;
        }
        let _session_guard = PlaybackSessionGuard {
            app: app.clone(),
            generation,
        };

        let websocket_url = match discover_cdp_target(port) {
            Ok(url) => url,
            Err(error) => {
                playback_failed(&app, generation, &error);
                return;
            }
        };
        // The validated WebSocket URL is loopback-only. Connect with an explicit
        // TCP timeout and bound all subsequent reads/writes so Stop can recover.
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = match TcpStream::connect_timeout(&address, CDP_CONNECT_TIMEOUT) {
            Ok(stream) => stream,
            Err(error) => {
                playback_failed(
                    &app,
                    generation,
                    &format!("Could not connect to WebView2 input: {error}"),
                );
                return;
            }
        };
        if let Err(error) = stream
            .set_read_timeout(Some(CDP_IO_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(CDP_IO_TIMEOUT)))
        {
            playback_failed(
                &app,
                generation,
                &format!("Could not configure WebView2 input timeouts: {error}"),
            );
            return;
        }
        let (mut socket, _) = match tungstenite::client(websocket_url.as_str(), stream) {
            Ok(connection) => connection,
            Err(error) => {
                playback_failed(
                    &app,
                    generation,
                    &format!("Could not complete WebView2 input handshake: {error}"),
                );
                return;
            }
        };
        let mut next_id = 1_u64;
        if let Err(error) = viewport_size(&mut socket, &mut next_id) {
            playback_failed(&app, generation, &error);
            return;
        }
        let Some(state) = app.try_state::<MacroState>() else {
            return;
        };
        if !playback_session_is_current(
            state.mode.load(Ordering::SeqCst),
            generation,
            state.playback_generation.load(Ordering::SeqCst),
        ) {
            return;
        }
        notify(
            &app,
            "BFR Macro — Playback started",
            "The macro is running independently of your physical mouse and keyboard.",
            false,
        );

        let mut completed_normally = true;
        'playback: loop {
            let loop_started = Instant::now();
            for click in &clicks {
                let Some(state) = app.try_state::<MacroState>() else {
                    return;
                };
                let Some(deadline) = playback_click_deadline(loop_started, click.elapsed_ms)
                else {
                    playback_failed(
                        &app,
                        generation,
                        "The macro timing exceeded the supported duration.",
                    );
                    return;
                };
                if !wait_while_playing_until(&state, generation, deadline) {
                    completed_normally = false;
                    break 'playback;
                }
                if !playback_session_is_current(
                    state.mode.load(Ordering::SeqCst),
                    generation,
                    state.playback_generation.load(Ordering::SeqCst),
                ) {
                    completed_normally = false;
                    break 'playback;
                }
                let (width, height) = match viewport_size(&mut socket, &mut next_id) {
                    Ok(size) => size,
                    Err(error) => {
                        playback_failed(&app, generation, &error);
                        return;
                    }
                };
                let (x, y) = playback_click_position(click, width, height);
                if let Err(error) = dispatch_click(&mut socket, &mut next_id, x, y) {
                    playback_failed(&app, generation, &error);
                    return;
                }
            }

            let Some(state) = app.try_state::<MacroState>() else {
                return;
            };
            if !playback_session_is_current(
                state.mode.load(Ordering::SeqCst),
                generation,
                state.playback_generation.load(Ordering::SeqCst),
            ) || !state.loop_enabled.load(Ordering::SeqCst)
            {
                break;
            }
            let last_elapsed_ms = clicks.last().map(|click| click.elapsed_ms).unwrap_or_default();
            let Some(next_loop_deadline) =
                playback_next_loop_deadline(loop_started, last_elapsed_ms)
            else {
                playback_failed(
                    &app,
                    generation,
                    "The macro loop timing exceeded the supported duration.",
                );
                return;
            };
            if !wait_while_playing_until(&state, generation, next_loop_deadline) {
                completed_normally = false;
                break;
            }
        }

        if completed_normally {
            if let Some(state) = app.try_state::<MacroState>() {
                if state.playback_generation.load(Ordering::SeqCst) == generation
                    && state
                        .mode
                        .compare_exchange(
                            MODE_PLAYING,
                            MODE_IDLE,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                {
                    state.performance_event(
                        "playback_finished",
                        json!({ "click_count": clicks.len() }),
                    );
                    refresh_tray_menu(&app);
                    notify(
                        &app,
                        "BFR Macro — Playback finished",
                        "The macro completed its playback.",
                        false,
                    );
                }
            }
        }
    });
}

fn set_webview_memory_target(app: &AppHandle<Wry>, window: &WebviewWindow<Wry>, low: bool) {
    #[cfg(windows)]
    {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        };
        // `cast()` is provided by the windows-core Interface trait. It must
        // be imported explicitly for the locked windows-core 0.62.2 API.
        use windows_core::Interface;

        let target = if low {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
        } else {
            COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
        };
        let result = Arc::new(Mutex::new(None::<String>));
        let result_for_webview = result.clone();
        let invoke_result = window.with_webview(move |webview| {
            let change_result = (|| -> Result<(), String> {
                // Tauri exposes the WebView2 controller on PlatformWebview rather
                // than Wry's WebViewExtWindows trait. Query ICoreWebView2_19
                // directly so this works with the project's locked Tauri/Wry APIs.
                let core = unsafe { webview.controller().CoreWebView2() }
                    .map_err(|error| error.to_string())?;
                let core19 = core.cast::<ICoreWebView2_19>().map_err(|error| {
                    format!("WebView2 memory target API is unavailable: {error}")
                })?;
                unsafe { core19.SetMemoryUsageTargetLevel(target) }
                    .map_err(|error| error.to_string())
            })();
            if let Err(error) = change_result {
                if let Ok(mut slot) = result_for_webview.lock() {
                    *slot = Some(error);
                }
            }
        });
        let error = invoke_result
            .err()
            .map(|error| error.to_string())
            .or_else(|| result.lock().ok().and_then(|slot| slot.clone()));
        if let Some(error) = error {
            if let Some(state) = app.try_state::<MacroState>() {
                state.log(&format!(
                    "WebView2 memory target change to {} was unavailable: {error}",
                    if low { "Low" } else { "Normal" }
                ));
                state.performance_event(
                    "webview_memory_target_failed",
                    json!({ "target": if low { "low" } else { "normal" }, "error": error }),
                );
            }
        } else if let Some(state) = app.try_state::<MacroState>() {
            state.performance_event(
                "webview_memory_target_changed",
                json!({ "target": if low { "low" } else { "normal" } }),
            );
        }
    }
    #[cfg(not(windows))]
    let _ = (app, window, low);
}

fn show_game(app: &AppHandle<Wry>, window: &WebviewWindow<Wry>) {
    set_webview_memory_target(app, window, false);
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

fn find_free_local_port() -> Result<u16, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    drop(listener);
    Ok(port)
}

fn icon_item(
    app: &AppHandle<Wry>,
    id: &str,
    label: &str,
    png: &'static [u8],
) -> tauri::Result<IconMenuItem<Wry>> {
    let icon = Image::from_bytes(png)?;
    IconMenuItem::with_id(app, id, label, true, Some(icon), None::<&str>)
}

fn migrate_legacy_file(legacy: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() || !legacy.exists() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    // Copy rather than move so the original 0.5.1 data remains as a rollback copy.
    fs::copy(legacy, destination)?;
    Ok(())
}

fn migrate_tracker_history(legacy: &Path, destination: &Path) -> std::io::Result<()> {
    if !legacy.exists() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let recovery_copy = legacy.with_extension("jsonl.migrated");
    if !destination.exists() {
        fs::copy(legacy, destination)?;
        if !recovery_copy.exists() {
            fs::rename(legacy, recovery_copy)?;
        }
        return Ok(());
    }

    // Byte-based deduplication tolerates malformed UTF-8 and avoids the old
    // O(legacy_lines × destination_lines) repeated scan. Keep a recovery copy.
    let mut existing_lines: HashSet<Vec<u8>> = HashSet::new();
    let destination_file = fs::File::open(destination)?;
    for line in std::io::BufReader::new(destination_file).split(b'\n') {
        let mut line = line?;
        while line.last().is_some_and(|last| *last == b'\r') {
            line.pop();
        }
        if !line.is_empty() {
            existing_lines.insert(line);
        }
    }
    let legacy_file = fs::File::open(legacy)?;
    let mut output = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(destination)?;
    let mut appended_any = false;
    for line in std::io::BufReader::new(legacy_file).split(b'\n') {
        let mut line = line?;
        while line.last().is_some_and(|last| *last == b'\r') {
            line.pop();
        }
        if line.is_empty() || !existing_lines.insert(line.clone()) {
            continue;
        }
        if !appended_any {
            // Separate imported records from a pre-existing torn final line,
            // preserving that line's bytes rather than concatenating records.
            ensure_jsonl_line_boundary(&mut output)?;
            appended_any = true;
        }
        output.write_all(&line)?;
        output.write_all(b"\n")?;
    }
    output.flush()?;
    output.sync_all()?;
    // Preserve the original bytes under a recovery suffix and stop re-importing
    // the legacy path on every launch. If a prior recovery copy exists, leave the
    // source in place; line deduplication still makes a later retry idempotent.
    if !recovery_copy.exists() {
        fs::rename(legacy, recovery_copy)?;
    }
    Ok(())
}

// Preserve the historical marker path for upgrade compatibility, but never clear
// user history as part of startup. The marker lives outside Logs and survives cleanup.
fn reset_run_history_once(app_data_dir: &Path, _history_file: &Path) -> Result<bool, String> {
    let marker_file = app_data_dir
        .join("Settings")
        .join("run-history-reset-v1.5.1-dev.3.done");
    if marker_file.exists() {
        return Ok(false);
    }

    // The old dev-only safeguard erased history on first launch for users who
    // never ran that development build. Mark the safeguard as handled without
    // deleting the active history or its legacy rotation backup.
    write_file_atomically(
        &marker_file,
        b"Non-destructive safeguard retained existing Run History.\n",
    )?;
    Ok(true)
}

fn open_data_folder(app: &AppHandle<Wry>, folder: PathBuf, label: &str) {
    if let Err(error) = fs::create_dir_all(&folder) {
        notify(
            app,
            "BFR Macro — Folder unavailable",
            &format!("Could not create the {label} folder: {error}"),
            true,
        );
        return;
    }
    match Command::new("explorer.exe").arg(&folder).spawn() {
        Ok(_) => {}
        Err(error) => notify(
            app,
            "BFR Macro — Could not open folder",
            &format!("Could not open the {label} folder: {error}"),
            true,
        ),
    }
}

fn open_tracker_window(app: &AppHandle<Wry>) {
    if let Some(existing) = app.get_webview_window("tracker") {
        let _ = existing.show();
        let _ = existing.set_focus();
        return;
    }
    let (width, height, pinned) = app
        .try_state::<SettingsState>()
        .and_then(|state| {
            state
                .settings
                .lock()
                .ok()
                .map(|s| (s.tracker_width, s.tracker_height, s.tracker_pinned))
        })
        .unwrap_or((760.0, 820.0, false));
    let profile_dir = app
        .path()
        .app_data_dir()
        .ok()
        .map(|path| path.join("webview2-profile"));
    let mut builder =
        WebviewWindowBuilder::new(app, "tracker", WebviewUrl::App("tracker.html".into()))
            .title("BFR Macro — Run History")
            .inner_size(width, height)
            .min_inner_size(560.0, 600.0)
            .resizable(true)
            .always_on_top(pinned)
            .center();
    if let Some(profile_dir) = profile_dir {
        builder = builder.data_directory(profile_dir);
    }
    match builder.build() {
        Ok(window) => {
            let handle = app.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Resized(size) = event {
                    if size.width == 0 || size.height == 0 { return; }
                    let scale = handle.get_webview_window("tracker")
                        .and_then(|w| w.scale_factor().ok())
                        .filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0);
                    if let Some(state) = handle.try_state::<SettingsState>() {
                        if let Ok(mut settings) = state.settings.lock() {
                            settings.tracker_width = size.width as f64 / scale;
                            settings.tracker_height = size.height as f64 / scale;
                        }
                        // Coalesce resize drags through the same 400 ms background
                        // saver used by the main game window; never sync disk here.
                        if state.resize_save_tx.send(()).is_err() {
                            if let Some(macro_state) = handle.try_state::<MacroState>() {
                                macro_state.log("Could not queue the resized Run History window dimensions for saving.");
                            }
                        }
                    }
                }
            });
        }
        Err(error) => notify(
            app,
            "BFR Run History — Could not open",
            &error.to_string(),
            true,
        ),
    }
}

fn build_tray(app: &AppHandle<Wry>) -> tauri::Result<()> {
    let open = icon_item(
        app,
        "open",
        "Open Game",
        include_bytes!("../icons/menu/monitor.png"),
    )?;
    let tracker = icon_item(
        app,
        "open_tracker",
        "Run History",
        include_bytes!("../icons/menu/history.png"),
    )?;
    let minimize = icon_item(
        app,
        "minimize",
        "Minimize to Tray",
        include_bytes!("../icons/menu/minimize.png"),
    )?;
    let reset = icon_item(
        app,
        "reset_size",
        "Reset Window Size",
        include_bytes!("../icons/menu/reset.png"),
    )?;
    let pin = Arc::new(icon_item(
        app,
        "toggle_pin",
        "Pin Window",
        include_bytes!("../icons/menu/pin.png"),
    )?);
    let separator = PredefinedMenuItem::separator(app)?;
    let record = Arc::new(icon_item(
        app,
        "record",
        "Start Recording",
        include_bytes!("../icons/menu/record.png"),
    )?);
    let play = Arc::new(icon_item(
        app,
        "play",
        "Play Macro",
        include_bytes!("../icons/menu/play.png"),
    )?);
    let loop_item = Arc::new(icon_item(
        app,
        "loop",
        "Loop Continuously: On",
        include_bytes!("../icons/menu/loop.png"),
    )?);
    let load = icon_item(
        app,
        "load",
        "Load Last Saved Macro",
        include_bytes!("../icons/menu/folder.png"),
    )?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let app_folder = icon_item(
        app,
        "open_app_folder",
        "Open Application Folder",
        include_bytes!("../icons/menu/folder.png"),
    )?;
    let settings_folder = icon_item(
        app,
        "open_settings_folder",
        "Open Settings Folder",
        include_bytes!("../icons/menu/folder.png"),
    )?;
    let macros_folder = icon_item(
        app,
        "open_macros_folder",
        "Open Saved Macros Folder",
        include_bytes!("../icons/menu/folder.png"),
    )?;
    let logs_folder = icon_item(
        app,
        "open_logs_folder",
        "Open Logs Folder",
        include_bytes!("../icons/menu/folder.png"),
    )?;
    let separator3 = PredefinedMenuItem::separator(app)?;
    let exit = icon_item(
        app,
        "exit",
        "Exit BFR Macro & Tracking Utility",
        include_bytes!("../icons/menu/exit.png"),
    )?;

    let items: [&dyn IsMenuItem<Wry>; 17] = [
        &open,
        &reset,
        &minimize,
        pin.as_ref(),
        &tracker,
        &separator,
        record.as_ref(),
        play.as_ref(),
        loop_item.as_ref(),
        &load,
        &separator2,
        &app_folder,
        &settings_folder,
        &macros_folder,
        &logs_folder,
        &separator3,
        &exit,
    ];
    let menu = Menu::with_items(app, &items)?;
    app.manage(TrayMenuState {
        record: record.clone(),
        play: play.clone(),
        loop_item: loop_item.clone(),
        pin: pin.clone(),
    });

    TrayIconBuilder::with_id("bfr-macro-tray")
        .icon(
            app.default_window_icon()
                .cloned()
                .expect("default app icon is configured"),
        )
        .tooltip("BFR Macro & Tracking Utility")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let window = app.get_webview_window("game");
            match event.id().as_ref() {
                "open" => {
                    if let Some(window) = window.as_ref() {
                        show_game(app, window);
                    }
                }
                "open_tracker" => {
                    open_tracker_window(app);
                }
                "minimize" => {
                    if let Some(window) = window.as_ref() {
                        set_webview_memory_target(app, window, true);
                        let _ = window.hide();
                    }
                }
                "reset_size" => {
                    if let Some(window) = window.as_ref() {
                        let _ =
                            window.set_size(tauri::LogicalSize::new(WINDOW_WIDTH, WINDOW_HEIGHT));
                        let _ = window.center();
                    }
                    if let Some(state) = app.try_state::<SettingsState>() {
                        if let Ok(mut settings) = state.settings.lock() {
                            settings.width = WINDOW_WIDTH;
                            settings.height = WINDOW_HEIGHT;
                        }
                        if let Err(error) = state.save() {
                            if let Some(macro_state) = app.try_state::<MacroState>() {
                                macro_state
                                    .log(&format!("Could not save reset window size: {error}"));
                            }
                            notify(
                                app,
                                "BFR Macro — Settings save failed",
                                &format!("Could not save window settings: {error}"),
                                true,
                            );
                        }
                    }
                    notify(app, "BFR Macro", "Window size reset.", false);
                }
                "toggle_pin" => {
                    if let Some(window) = window.as_ref() {
                        if let Some(state) = app.try_state::<SettingsState>() {
                            let pinned = if let Ok(mut settings) = state.settings.lock() {
                                settings.pinned = !settings.pinned;
                                settings.pinned
                            } else {
                                false
                            };
                            let _ = window.set_always_on_top(pinned);
                            if let Err(error) = state.save() {
                                notify(
                                    app,
                                    "BFR Macro — Settings save failed",
                                    &format!("Could not save window pin setting: {error}"),
                                    true,
                                );
                            }
                            refresh_tray_menu(app);
                            notify(
                                app,
                                "BFR Macro",
                                if pinned {
                                    "Window pin enabled."
                                } else {
                                    "Window pin disabled."
                                },
                                false,
                            );
                        }
                    }
                }
                "record" => start_recording(app),
                "play" => start_playback(app.clone()),
                "loop" => {
                    if let Some(state) = app.try_state::<MacroState>() {
                        let enabled = !state.loop_enabled.load(Ordering::SeqCst);
                        state.loop_enabled.store(enabled, Ordering::SeqCst);
                        if let Some(settings) = app.try_state::<SettingsState>() {
                            if let Ok(mut value) = settings.settings.lock() {
                                value.loop_enabled = enabled;
                            }
                            if let Err(error) = settings.save() {
                                notify(
                                    app,
                                    "BFR Macro — Settings save failed",
                                    &format!("Could not save loop setting: {error}"),
                                    true,
                                );
                            }
                        }
                        refresh_tray_menu(app);
                        notify(
                            app,
                            "BFR Macro",
                            if enabled {
                                "Continuous looping enabled."
                            } else {
                                "Continuous looping disabled."
                            },
                            false,
                        );
                    }
                }
                "load" => {
                    if let Some(state) = app.try_state::<MacroState>() {
                        match state.load_macro() {
                            Ok(count) => {
                                refresh_tray_menu(app);
                                notify(
                                    app,
                                    "BFR Macro",
                                    &format!("Loaded the last saved macro ({count} clicks)."),
                                    false,
                                );
                            }
                            Err(error) => notify(
                                app,
                                "BFR Macro — Load failed",
                                &format!("Could not load a saved macro: {error}"),
                                true,
                            ),
                        }
                    }
                }
                "open_app_folder" => match std::env::current_exe() {
                    Ok(executable) => {
                        if let Some(folder) = executable.parent() {
                            open_data_folder(app, folder.to_path_buf(), "application");
                        }
                    }
                    Err(error) => notify(
                        app,
                        "BFR Macro — Could not locate application",
                        &error.to_string(),
                        true,
                    ),
                },
                "open_settings_folder" => {
                    if let Ok(app_data) = app.path().app_data_dir() {
                        open_data_folder(app, app_data.join("Settings"), "settings");
                    }
                }
                "open_macros_folder" => {
                    if let Ok(app_data) = app.path().app_data_dir() {
                        open_data_folder(app, app_data.join("Macros"), "saved macros");
                    }
                }
                "open_logs_folder" => {
                    if let Ok(app_data) = app.path().app_data_dir() {
                        open_data_folder(app, app_data.join("Logs"), "logs");
                    }
                }
                "exit" => {
                    stop_recording(app);
                    stop_playback(app);
                    save_settings_on_exit(app);
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("game") {
                    show_game(app, &window);
                }
            }
        })
        .build(app)?;

    refresh_tray_menu(app);
    Ok(())
}

fn save_settings_on_exit(app: &AppHandle<Wry>) {
    if let Some(state) = app.try_state::<MacroState>() {
        state.performance_event("application_exit_requested", json!({}));
    }
    if let Some(settings) = app.try_state::<SettingsState>() {
        if let Err(error) = settings.save() {
            if let Some(state) = app.try_state::<MacroState>() {
                state.log(&format!("Could not save settings during shutdown: {error}"));
            }
        }
    }
}

#[cfg(windows)]
fn ensure_single_instance() -> Result<bool, String> {
    use std::{ffi::c_void, sync::OnceLock};

    const ERROR_ALREADY_EXISTS: u32 = 183;
    static INSTANCE_MUTEX: OnceLock<isize> = OnceLock::new();

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            attributes: *mut c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let name: Vec<u16> = r"Local\BFRMacroUtility.SingleInstance"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error().to_string());
    }

    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return Ok(false);
    }

    INSTANCE_MUTEX
        .set(handle as isize)
        .map_err(|_| "Could not retain the application instance lock".to_string())?;
    Ok(true)
}

#[cfg(not(windows))]
fn ensure_single_instance() -> Result<bool, String> {
    Ok(true)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            recording_bridge_ready,
            tracker_diagnostic_event,
            tracker_dungeon_context,
            tracker_reward_event,
            tracker_read_data_page,
            tracker_clear_history,
            tracker_get_pinned,
            tracker_set_pinned,
            record_click,
            report_recording_error
        ])
        .setup(|app| {
            if !ensure_single_instance().map_err(std::io::Error::other)? {
                // The existing instance owns the shared macro/settings files.
                app.handle().exit(0);
                return Ok(());
            }

            let app_data = app.path().app_data_dir()?;
            let settings_dir = app_data.join("Settings");
            let macros_dir = app_data.join("Macros");
            let logs_dir = app_data.join("Logs");
            fs::create_dir_all(&settings_dir)?;
            fs::create_dir_all(&macros_dir)?;
            fs::create_dir_all(&logs_dir)?;
            let tracker_migration_error = migrate_tracker_history(
                &app_data.join("tracker-data").join("battle-rewards.jsonl"),
                &logs_dir.join("battle-rewards.jsonl"),
            ).err();

            // One-time, non-destructive migration from the original 0.5.1 data layout.
            // Migration failures are recorded and startup continues; originals are copied,
            // never moved or deleted. Settings writes are separately guarded below.
            let legacy_settings_file = app_data.join("settings.json");
            let settings_file = settings_dir.join("settings.json");
            let settings_migration_error = migrate_legacy_file(&legacy_settings_file, &settings_file).err();
            let macro_migration_error = migrate_legacy_file(&app_data.join("recording.bfrmacro"), &macros_dir.join("recording.bfrmacro")).err();
            let log_migration_error = migrate_legacy_file(&app_data.join("bfr-macro.log"), &logs_dir.join("bfr-macro.log")).err();

            // Prefer the new location, but if migration failed and only the legacy
            // file exists, read it without changing it. Any uncertain/corrupt state
            // blocks subsequent saves so defaults cannot destroy the original.
            let settings_source = if settings_file.exists() { &settings_file } else { &legacy_settings_file };
            let (mut settings, settings_save_blocked_reason) = if settings_source.exists() {
                match fs::read(settings_source) {
                    Ok(bytes) => match serde_json::from_slice::<AppSettings>(&bytes) {
                        Ok(value) => (value, settings_migration_error.as_ref().map(|error| format!("legacy settings migration failed: {error}"))),
                        Err(error) => (AppSettings::default(), Some(format!("existing settings file could not be parsed: {error}"))),
                    },
                    Err(error) => (AppSettings::default(), Some(format!("existing settings file could not be read: {error}"))),
                }
            } else {
                (AppSettings::default(), None)
            };
            if !settings.width.is_finite() || settings.width < 600.0 {
                settings.width = WINDOW_WIDTH;
            }
            if !settings.height.is_finite() || settings.height < 750.0 {
                settings.height = WINDOW_HEIGHT;
            }
            if !settings.tracker_width.is_finite() || settings.tracker_width < 560.0 { settings.tracker_width = 760.0; }
            if !settings.tracker_height.is_finite() || settings.tracker_height < 600.0 { settings.tracker_height = 820.0; }

            let profile_dir = app_data.join("webview2-profile");
            fs::create_dir_all(&profile_dir)?;
            let macro_file = macros_dir.join("recording.bfrmacro");
            let log_file = logs_dir.join("bfr-macro.log");
            let performance_file = logs_dir.join("performance.jsonl");
            let tracker_diagnostics_file = logs_dir.join("tracker-discovery.jsonl");
            let tracker_sessions_file = logs_dir.join("battle-rewards.jsonl");
            let tracker_context_file = logs_dir.join("tracker-context.jsonl");
            let tracker_legacy_file = app_data.join("tracker-data").join("battle-rewards.jsonl");
            let tracker_backup_file = tracker_sessions_file.with_extension("jsonl.1");
            let tracker_dedupe_keys = load_history_dedupe_keys(&tracker_backup_file, &tracker_sessions_file);
            let run_history_reset_result = reset_run_history_once(&app_data, &tracker_sessions_file);
            let run_history_reset = run_history_reset_result.as_ref().copied().unwrap_or(false);
            let run_history_marker_error = run_history_reset_result.err();
            let cdp_port = find_free_local_port().map_err(std::io::Error::other)?;
            let existing_args = std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").unwrap_or_default();
            // The Rust CDP client does not send a browser Origin header, so the
            // broad --remote-allow-origins=* exception is unnecessary. Keep the
            // debugging endpoint bound to loopback and validate its returned URL.
            let debugging_args = format!(
                "--remote-debugging-address=127.0.0.1 --remote-debugging-port={cdp_port}"
            );
            let combined_args = if existing_args.trim().is_empty() {
                debugging_args
            } else {
                format!("{existing_args} {debugging_args}")
            };
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", combined_args);

            let macro_state = MacroState {
                file: macro_file,
                log_file,
                performance_file: performance_file.clone(),
                tracker_diagnostics_file,
                tracker_sessions_file,
                tracker_context_file,
                tracker_legacy_file,
                tracker_data_lock: Mutex::new(()),
                tracker_dedupe_keys: Mutex::new(tracker_dedupe_keys.as_ref().cloned().unwrap_or_default()),
                tracker_diagnostic_rate_limit: Mutex::new(TrackerDiagnosticRateLimit::default()),
                cdp_port,
                mode: AtomicU8::new(MODE_IDLE),
                playback_generation: AtomicU64::new(0),
                bridge_ready: AtomicBool::new(false),
                record_started: Mutex::new(None),
                clicks: Mutex::new(Vec::new()),
                loop_enabled: AtomicBool::new(settings.loop_enabled),
            };
            if run_history_reset {
                macro_state.log("The one-time non-destructive Run History safeguard was marked; existing history, settings and saved macros were preserved.");
            }
            if let Some(error) = tracker_migration_error {
                macro_state.log(&format!("Legacy Run History migration could not complete; the original file was retained for recovery: {error}"));
            }
            if let Some(error) = settings_migration_error {
                macro_state.log(&format!("Legacy settings migration could not complete; the original file was retained: {error}"));
            }
            if let Some(error) = macro_migration_error {
                macro_state.log(&format!("Legacy macro migration could not complete; the original file was retained: {error}"));
            }
            if let Some(error) = log_migration_error {
                macro_state.log(&format!("Legacy application log migration could not complete; the original file was retained: {error}"));
            }
            if let Some(reason) = settings_save_blocked_reason.as_deref() {
                macro_state.log(&format!("Settings persistence is temporarily blocked to protect existing user data: {reason}"));
            }
            if let Some(error) = run_history_marker_error {
                macro_state.log(&format!("Could not write the non-destructive Run History marker; startup continued without changing history: {error}"));
            }
            if let Err(error) = tracker_dedupe_keys {
                macro_state.log(&format!("Could not pre-load Run History duplicate keys; new events will still be recorded: {error}"));
            }
            if macro_state.file.exists() {
                if let Err(error) = macro_state.load_macro() {
                    macro_state.log(&format!("Saved macro could not be loaded at startup: {error}"));
                }
            }
            let (resize_save_tx, resize_save_rx) = mpsc::channel::<()>();
            app.manage(SettingsState {
                file: settings_file,
                settings: Mutex::new(settings.clone()),
                save_lock: Mutex::new(()),
                save_blocked_reason: settings_save_blocked_reason,
                resize_save_tx,
            });
            app.manage(macro_state);

            // Structured, low-frequency diagnostics. This samples the app and its
            // WebView2 child-process tree once per minute and records lifecycle events.
            if let Some(state) = app.try_state::<MacroState>() {
                state.performance_event("application_started", json!({
                    "process_id": std::process::id(),
                    "sample_interval_seconds": 60,
                    "metrics_scope": "application_and_webview2_child_processes"
                }));
            }
            let metrics_app = app.handle().clone();
            let metrics_file = performance_file.clone();
            thread::spawn(move || {
                let mut previous_cpu: Option<(u64, Instant)> = None;
                let app_started = Instant::now();
                while let Some(state) = metrics_app.try_state::<MacroState>() {
                    let mode = state.mode.load(Ordering::SeqCst);
                    let click_count = state.clicks.lock().map(|clicks| clicks.len()).unwrap_or(0);
                    #[cfg(windows)]
                    let metrics = {
                        let sample = collect_process_metrics(&mut previous_cpu);
                        json!({
                            "app_webview_working_set_mb": sample.working_set_bytes.map(|v| v as f64 / 1_048_576.0),
                            "app_webview_private_mb": sample.private_bytes.map(|v| v as f64 / 1_048_576.0),
                            "app_webview_cpu_percent": sample.cpu_percent,
                            "system_memory_load_percent": sample.system_memory_load_percent,
                            "system_total_memory_mb": sample.system_total_memory_bytes.map(|v| v as f64 / 1_048_576.0),
                            "system_available_memory_mb": sample.system_available_memory_bytes.map(|v| v as f64 / 1_048_576.0),
                            "process_details": sample.process_details
                        })
                    };
                    #[cfg(not(windows))]
                    let metrics = collect_process_metrics(&mut previous_cpu);
                    let record = json!({
                        "schema_version": 1,
                        "record_type": "sample",
                        "timestamp_unix_ms": unix_timestamp_ms(),
                        "process_id": std::process::id(),
                        "app_uptime_seconds": app_started.elapsed().as_secs(),
                        "activity": activity_name(mode),
                        "loaded_or_recorded_macro_click_count": click_count,
                        "metrics": metrics
                    });
                    if let Err(error) = append_performance_record(&metrics_file, &record) {
                        state.log(&format!("Performance sample could not be written: {error}"));
                    }
                    thread::sleep(Duration::from_secs(60));
                }
            });

            // Coalesce the many resize events generated while dragging the window.
            // Only the final size is written after a short quiet period.
            let resize_save_app = app.handle().clone();
            thread::spawn(move || {
                while resize_save_rx.recv().is_ok() {
                    loop {
                        match resize_save_rx.recv_timeout(Duration::from_millis(400)) {
                            Ok(()) => continue,
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let Some(settings) = resize_save_app.try_state::<SettingsState>() else {
                        break;
                    };
                    if let Err(error) = settings.save() {
                        if let Some(state) = resize_save_app.try_state::<MacroState>() {
                            state.log(&format!("Could not save resized window dimensions: {error}"));
                        }
                    }
                }
            });

            let game_url = GAME_URL.parse().expect("GAME_URL must be a valid URL");
            let window = WebviewWindowBuilder::new(app, "game", WebviewUrl::External(game_url))
                .title("BFR Macro & Tracking Utility")
                .inner_size(settings.width, settings.height)
                .min_inner_size(600.0, 750.0)
                .resizable(true)
                .center()
                .always_on_top(settings.pinned)
                .data_directory(profile_dir)
                .initialization_script(INJECTION_SCRIPT)
                .build()?;

            set_webview_memory_target(app.handle(), &window, false);
            build_tray(app.handle())?;
            notify(app.handle(), "BFR Macro", "BFR Macro is ready. Wait for the game to finish loading before starting a recording.", false);

            let app_handle = app.handle().clone();
            window.on_window_event(move |event| match event {
                tauri::WindowEvent::CloseRequested { .. } => {
                    stop_recording(&app_handle);
                    stop_playback(&app_handle);
                    save_settings_on_exit(&app_handle);
                    app_handle.exit(0);
                }
                tauri::WindowEvent::Resized(size) => {
                    if let Some(window) = app_handle.get_webview_window("game") {
                        if window.is_minimized().unwrap_or(false) {
                            set_webview_memory_target(&app_handle, &window, true);
                            let _ = window.hide();
                            return;
                        }
                    }
                    if size.width > 0 && size.height > 0 {
                        if let Some(state) = app_handle.try_state::<SettingsState>() {
                            // Tauri resize events provide physical pixels, while
                            // WebviewWindowBuilder::inner_size expects logical pixels.
                            // Persisting logical dimensions prevents scaling drift on
                            // Windows displays configured above or below 100% DPI.
                            let scale_factor = app_handle
                                .get_webview_window("game")
                                .and_then(|window| window.scale_factor().ok())
                                .filter(|factor| factor.is_finite() && *factor > 0.0)
                                .unwrap_or(1.0);
                            if let Ok(mut settings) = state.settings.lock() {
                                settings.width = size.width as f64 / scale_factor;
                                settings.height = size.height as f64 / scale_factor;
                            }
                            // The background worker saves once resizing has been
                            // quiet for 400 ms, avoiding repeated disk writes.
                            if state.resize_save_tx.send(()).is_err() {
                                if let Some(macro_state) = app_handle.try_state::<MacroState>() {
                                    macro_state.log("Could not queue the resized window settings for saving.");
                                }
                            }
                        }
                    }
                }
                _ => {}
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running BFR Macro & Tracking Utility");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_diagnostic_rate_limit_caps_events_and_resets_after_window() {
        let mut limiter = TrackerDiagnosticRateLimit::default();
        for _ in 0..MAX_TRACKER_DIAGNOSTICS_PER_WINDOW {
            assert!(limiter.allow());
        }
        assert!(!limiter.allow());
        limiter.accepted_at = vec![
            Instant::now() - Duration::from_secs(61);
            MAX_TRACKER_DIAGNOSTICS_PER_WINDOW as usize
        ]
        .into();
        assert!(limiter.allow());
        assert_eq!(limiter.accepted_at.len(), 1);
    }

    #[test]
    fn recording_error_message_is_bounded_and_removes_control_characters() {
        let input = format!("capture\nfailed\r{}", "x".repeat(700));
        let safe = sanitize_recording_error_message(&input);
        assert!(!safe.chars().any(char::is_control));
        assert_eq!(safe.chars().count(), MAX_RECORDING_ERROR_MESSAGE_CHARS);
        assert_eq!(
            sanitize_recording_error_message("\n\r\t"),
            "Unknown recording bridge error"
        );
    }

    fn temp_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("bfr-{label}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn repeated_atomic_save_failures_preserve_existing_data_and_clean_temporary_files() {
        let dir = temp_dir("atomic-save-failure");
        let destination = dir.join("settings.json");
        fs::create_dir(&destination).unwrap();
        let sentinel = destination.join("keep.txt");
        fs::write(&sentinel, b"original data").unwrap();

        for attempt in 0..10 {
            assert!(
                write_file_atomically(&destination, b"replacement").is_err(),
                "attempt {attempt} should fail"
            );
            assert_eq!(fs::read(&sentinel).unwrap(), b"original data");
            assert_eq!(
                fs::read_dir(&dir).unwrap().count(),
                1,
                "attempt {attempt} must not leave a temporary file"
            );
        }

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn legacy_reset_marker_never_erases_existing_history_or_backup() {
        let dir = temp_dir("history-reset");
        let history = dir.join("Logs/battle-rewards.jsonl");
        let backup = history.with_extension("jsonl.1");
        fs::create_dir_all(history.parent().unwrap()).unwrap();
        fs::write(&history, b"{\"record_type\":\"battle_reward\"}\n").unwrap();
        fs::write(&backup, b"older-history\n").unwrap();
        assert!(reset_run_history_once(&dir, &history).unwrap());
        assert_eq!(
            fs::read(&history).unwrap(),
            b"{\"record_type\":\"battle_reward\"}\n"
        );
        assert_eq!(fs::read(&backup).unwrap(), b"older-history\n");
        assert!(!reset_run_history_once(&dir, &history).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repeat_run_count_uses_cumulative_fields_not_chunk_runs() {
        let settlement = json!({"runs": 10, "wins": 10, "settled_runs": 20});
        let totals = json!({"runs": 20, "wins": 20, "settled_runs": 20});
        assert_eq!(repeat_cumulative_run_count(&settlement, &totals), Some(20));
        let chunk_only = json!({"runs": 10, "wins": 10});
        assert_eq!(repeat_cumulative_run_count(&chunk_only, &Value::Null), None);
    }

    #[test]
    fn macro_validation_rejects_absurd_delays_and_click_counts() {
        let absurd = vec![MacroClick {
            x: 0.5,
            y: 0.5,
            elapsed_ms: u64::MAX,
        }];
        assert!(validate_macro_clicks(&absurd).is_err());
        let too_many = vec![
            MacroClick {
                x: 0.5,
                y: 0.5,
                elapsed_ms: 0
            };
            MAX_MACRO_CLICKS + 1
        ];
        assert!(validate_macro_clicks(&too_many).is_err());
    }

    #[test]
    fn macro_loading_is_allowed_only_when_idle() {
        assert!(can_load_macro(MODE_IDLE));
        assert!(!can_load_macro(MODE_RECORDING));
        assert!(!can_load_macro(MODE_PLAYING));
        assert!(!can_load_macro(MODE_STOPPING));
    }

    #[test]
    fn macro_buffer_transitions_are_serialized_with_mode_changes() {
        let mode = AtomicU8::new(MODE_IDLE);
        let record_started = Mutex::new(None);
        let clicks = Mutex::new(vec![MacroClick {
            x: 0.5,
            y: 0.5,
            elapsed_ms: 10,
        }]);

        assert!(begin_recording_if_idle(&mode, &record_started, &clicks).unwrap());
        assert_eq!(mode.load(Ordering::SeqCst), MODE_RECORDING);
        assert!(clicks.lock().unwrap().is_empty());
        assert!(record_started.lock().unwrap().is_some());
        assert!(!can_load_macro(mode.load(Ordering::SeqCst)));

        mode.store(MODE_IDLE, Ordering::SeqCst);
        clicks.lock().unwrap().push(MacroClick {
            x: 0.25,
            y: 0.75,
            elapsed_ms: 20,
        });
        let snapshot = begin_playback_if_idle(&mode, &clicks).unwrap().unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(mode.load(Ordering::SeqCst), MODE_PLAYING);
        assert!(!can_load_macro(mode.load(Ordering::SeqCst)));

        let before = clicks.lock().unwrap().clone();
        assert!(!begin_recording_if_idle(&mode, &record_started, &clicks).unwrap());
        let after = clicks.lock().unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0].elapsed_ms, before[0].elapsed_ms);
    }

    #[test]
    fn recording_cannot_restart_or_replace_buffer_until_stop_save_finishes() {
        let mode = AtomicU8::new(MODE_IDLE);
        let record_started = Mutex::new(None);
        let clicks = Mutex::new(Vec::new());
        assert!(begin_recording_if_idle(&mode, &record_started, &clicks).unwrap());
        clicks.lock().unwrap().push(MacroClick {
            x: 0.2,
            y: 0.3,
            elapsed_ms: 10,
        });

        let (snapshot, _) = begin_stopping_recording(&mode, &record_started, &clicks)
            .unwrap()
            .unwrap();
        assert_eq!(mode.load(Ordering::SeqCst), MODE_STOPPING);
        assert_eq!(snapshot.len(), 1);
        assert!(!can_load_macro(mode.load(Ordering::SeqCst)));
        assert!(!begin_recording_if_idle(&mode, &record_started, &clicks).unwrap());
        assert!(begin_playback_if_idle(&mode, &clicks).unwrap().is_none());
        assert_eq!(clicks.lock().unwrap()[0].elapsed_ms, 10);

        // Hold a simulated save operation open on another thread. While it is
        // blocked, attempts to start/load/play must remain rejected.
        let mode = Arc::new(mode);
        let save_entered = Arc::new(std::sync::Barrier::new(2));
        let allow_save_finish = Arc::new(std::sync::Barrier::new(2));
        let worker_mode = Arc::clone(&mode);
        let worker_entered = Arc::clone(&save_entered);
        let worker_release = Arc::clone(&allow_save_finish);
        let worker = std::thread::spawn(move || {
            worker_entered.wait();
            worker_release.wait();
            assert!(finish_stopping_recording(&worker_mode));
        });
        save_entered.wait();
        assert_eq!(snapshot[0].elapsed_ms, 10);
        assert!(!begin_recording_if_idle(&mode, &record_started, &clicks).unwrap());
        assert!(begin_playback_if_idle(&mode, &clicks).unwrap().is_none());
        assert!(!can_load_macro(mode.load(Ordering::SeqCst)));
        allow_save_finish.wait();
        worker.join().unwrap();
        assert_eq!(mode.load(Ordering::SeqCst), MODE_IDLE);
        assert!(begin_recording_if_idle(&mode, &record_started, &clicks).unwrap());
        assert!(clicks.lock().unwrap().is_empty());
    }

    #[test]
    fn recording_error_stop_does_not_end_playback_or_idle_state() {
        let mode = AtomicU8::new(MODE_PLAYING);
        let started = Mutex::new(None);
        let clicks = Mutex::new(Vec::new());
        assert!(begin_stopping_recording(&mode, &started, &clicks)
            .unwrap()
            .is_none());
        assert_eq!(mode.load(Ordering::SeqCst), MODE_PLAYING);
        mode.store(MODE_IDLE, Ordering::SeqCst);
        assert!(begin_stopping_recording(&mode, &started, &clicks)
            .unwrap()
            .is_none());
        assert_eq!(mode.load(Ordering::SeqCst), MODE_IDLE);
    }

    #[test]
    fn blocked_settings_save_preserves_the_original_corrupt_file() {
        let dir = temp_dir("settings-preservation");
        let file = dir.join("settings.json");
        let original = b"{ this is intentionally malformed settings";
        fs::write(&file, original).unwrap();
        let (tx, _rx) = mpsc::channel();
        let state = SettingsState {
            file: file.clone(),
            settings: Mutex::new(AppSettings::default()),
            save_lock: Mutex::new(()),
            save_blocked_reason: Some("existing settings could not be parsed".to_string()),
            resize_save_tx: tx,
        };
        assert!(state.save().is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn synthetic_bfrmacro2_fixture_remains_deserializable() {
        // Synthetic compatibility fixture only; this is not evidence from an
        // authentic file produced by a historical application release.
        let fixture = br#"{"format":"BFRMACRO2","clicks":[{"x":0.25,"y":0.75,"elapsed_ms":125}]}"#;
        let parsed: MacroFile = serde_json::from_slice(fixture).unwrap();
        assert_eq!(parsed.format, "BFRMACRO2");
        assert_eq!(parsed.clicks.len(), 1);
        assert_eq!(parsed.clicks[0].elapsed_ms, 125);
        validate_macro_clicks(&parsed.clicks).unwrap();
    }

    #[test]
    fn macro_file_parser_accepts_both_supported_formats() {
        for format in ["BFRMACRO2", "BFRMACRO3"] {
            let fixture = format!(
                r#"{{"format":"{format}","clicks":[{{"x":0.25,"y":0.75,"elapsed_ms":125}}]}}"#
            );
            let parsed = parse_macro_file(fixture.as_bytes()).unwrap();
            assert_eq!(parsed.format, format);
            assert_eq!(parsed.clicks.len(), 1);
            assert_eq!(parsed.clicks[0].x, 0.25);
            assert_eq!(parsed.clicks[0].y, 0.75);
            assert_eq!(parsed.clicks[0].elapsed_ms, 125);
        }
    }

    #[test]
    fn macro_file_parser_rejects_unsupported_formats_and_invalid_clicks() {
        let unsupported = br#"{"format":"BFRMACRO1","clicks":[]}"#;
        assert_eq!(
            parse_macro_file(unsupported).unwrap_err(),
            "This macro file format is not supported"
        );

        let invalid_coordinates =
            br#"{"format":"BFRMACRO3","clicks":[{"x":1.5,"y":0.5,"elapsed_ms":0}]}"#;
        assert_eq!(
            parse_macro_file(invalid_coordinates).unwrap_err(),
            "The macro contains invalid click coordinates"
        );

        let out_of_order =
            br#"{"format":"BFRMACRO3","clicks":[{"x":0.5,"y":0.5,"elapsed_ms":200},{"x":0.5,"y":0.5,"elapsed_ms":100}]}"#;
        assert_eq!(
            parse_macro_file(out_of_order).unwrap_err(),
            "The macro contains click timestamps that are out of order"
        );

        assert!(parse_macro_file(b"not valid json").is_err());
    }

    #[test]
    fn macro_file_serialization_round_trips_as_current_format() {
        let data = MacroFile {
            format: "BFRMACRO3".to_string(),
            clicks: vec![
                MacroClick {
                    x: 0.25,
                    y: 0.75,
                    elapsed_ms: 125,
                },
                MacroClick {
                    x: 0.5,
                    y: 0.5,
                    elapsed_ms: 500,
                },
            ],
        };
        let bytes = serde_json::to_vec(&data).unwrap();
        let parsed = parse_macro_file(&bytes).unwrap();
        assert_eq!(parsed.format, "BFRMACRO3");
        assert_eq!(parsed.clicks.len(), 2);
        assert_eq!(parsed.clicks[1].elapsed_ms, 500);
    }

    #[test]
    fn playback_click_deadlines_are_relative_to_each_loop_start() {
        let loop_started = Instant::now();
        let first = playback_click_deadline(loop_started, 125).unwrap();
        let second = playback_click_deadline(loop_started, 375).unwrap();
        assert_eq!(first.duration_since(loop_started), Duration::from_millis(125));
        assert_eq!(second.duration_since(loop_started), Duration::from_millis(375));
        assert!(second > first);
    }

    #[test]
    fn playback_click_position_scales_normalized_coordinates_and_clamps_edges() {
        let center = MacroClick {
            x: 0.5,
            y: 0.25,
            elapsed_ms: 0,
        };
        assert_eq!(playback_click_position(&center, 800.0, 600.0), (400.0, 150.0));

        let bottom_right = MacroClick {
            x: 1.0,
            y: 1.0,
            elapsed_ms: 0,
        };
        assert_eq!(
            playback_click_position(&bottom_right, 800.0, 600.0),
            (799.0, 599.0)
        );
    }

    #[test]
    fn playback_loop_deadline_keeps_the_existing_half_second_gap() {
        let loop_started = Instant::now();
        let deadline = playback_next_loop_deadline(loop_started, 1_250).unwrap();
        assert_eq!(
            deadline.duration_since(loop_started),
            Duration::from_millis(1_750)
        );
    }

    #[test]
    fn playback_generation_invalidates_the_previous_worker() {
        assert!(playback_session_is_current(MODE_PLAYING, 7, 7));
        assert!(!playback_session_is_current(MODE_PLAYING, 7, 8));
        assert!(!playback_session_is_current(MODE_IDLE, 7, 7));
    }

    #[test]
    fn history_clear_stages_every_existing_source_before_cleanup() {
        let dir = temp_dir("history-clear-stage");
        let active = dir.join("battle-rewards.jsonl");
        let backup = dir.join("battle-rewards.jsonl.1");
        let legacy_dir = dir.join("tracker-data");
        fs::create_dir_all(&legacy_dir).unwrap();
        let legacy = legacy_dir.join("battle-rewards.jsonl");
        let migrated = legacy_dir.join("battle-rewards.jsonl.migrated");
        for (path, contents) in [
            (&active, b"active history".as_slice()),
            (&backup, b"backup history".as_slice()),
            (&legacy, b"legacy history".as_slice()),
            (&migrated, b"migrated history".as_slice()),
        ] {
            fs::write(path, contents).unwrap();
        }

        let quarantined = quarantine_history_files(&[
            active.clone(),
            backup.clone(),
            legacy.clone(),
            migrated.clone(),
        ])
        .unwrap();

        assert_eq!(quarantined.len(), 4);
        assert!(!active.exists());
        assert!(!backup.exists());
        assert!(!legacy.exists());
        assert!(!migrated.exists());
        for path in quarantined {
            assert!(path.exists());
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_clear_restores_staged_files_when_a_later_rename_fails() {
        let dir = temp_dir("history-clear-rollback");
        let first = dir.join("first.jsonl");
        let second = dir.join("second.jsonl");
        fs::write(&first, b"first history").unwrap();
        fs::write(&second, b"second history").unwrap();
        let mut rename_calls = 0;

        let result = quarantine_history_files_with(
            &[first.clone(), second.clone()],
            |source, destination| {
                rename_calls += 1;
                if rename_calls == 2 {
                    return Err(std::io::Error::other("simulated rename failure"));
                }
                fs::rename(source, destination)
            },
        );

        let error = result.unwrap_err();
        assert!(error.contains("original files were restored"));
        assert_eq!(fs::read(&first).unwrap(), b"first history");
        assert_eq!(fs::read(&second).unwrap(), b"second history");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_pages_are_bounded_ordered_and_stable_when_new_records_arrive() {
        let dir = temp_dir("history-pages");
        let backup = dir.join("battle-rewards.jsonl.1");
        let active = dir.join("battle-rewards.jsonl");
        fs::write(&backup, b"{\"n\":1}\n{bad-json}\n{\"n\":2}\n").unwrap();
        fs::write(&active, b"{\"n\":3}\n{\"n\":4}\n{\"n\":5}\n").unwrap();
        let paths = [backup.as_path(), active.as_path()];

        let (first, malformed) = read_jsonl_page(&paths, 0, 0, 2).unwrap();
        assert_eq!(malformed, 1);
        assert_eq!(first.total_records, 5);
        assert_eq!(first.records.iter().map(|r| r["n"].as_u64().unwrap()).collect::<Vec<_>>(), vec![4, 5]);
        assert_eq!(first.next_offset, 2);
        assert!(first.has_more);

        // A new completion arrives after the first page. The anchored cursor
        // must still return the two previously older records without a gap.
        append_history_record(&active, &json!({"n": 6})).unwrap();
        let (second, _) = read_jsonl_page(&paths, first.next_offset, first.total_records, 2).unwrap();
        assert_eq!(second.records.iter().map(|r| r["n"].as_u64().unwrap()).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(second.next_offset, 5);
        assert!(second.has_more);

        let (third, _) = read_jsonl_page(&paths, second.next_offset, second.total_records, 2).unwrap();
        assert_eq!(third.records.iter().map(|r| r["n"].as_u64().unwrap()).collect::<Vec<_>>(), vec![1]);
        assert!(!third.has_more);

        // Requests cannot force an unbounded response.
        let (bounded, _) = read_jsonl_page(&paths, 0, 0, usize::MAX).unwrap();
        assert!(bounded.records.len() <= MAX_HISTORY_PAGE_SIZE);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_history_line_is_skipped_without_hiding_later_records() {
        let dir = temp_dir("history-oversized-line");
        let history = dir.join("battle-rewards.jsonl");
        let mut contents = vec![b'x'; MAX_JSONL_LINE_BYTES + 128];
        contents.extend_from_slice(b"\n{\"record_type\":\"battle_reward\",\"n\":7}\n");
        fs::write(&history, contents).unwrap();

        let mut records = Vec::new();
        let mut malformed = 0;
        read_jsonl_records(&history, &mut records, &mut malformed).unwrap();

        assert_eq!(malformed, 1, "the oversized line must be counted and skipped");
        assert_eq!(records.len(), 1, "the valid record after it must still load");
        assert_eq!(records[0].get("n").and_then(Value::as_u64), Some(7));
    }

    #[test]
    fn history_line_at_exact_limit_is_accepted() {
        let dir = temp_dir("history-line-at-limit");
        let history = dir.join("battle-rewards.jsonl");
        let mut line = br#"{"record_type":"battle_reward","n":7}"#.to_vec();
        line.resize(MAX_JSONL_LINE_BYTES, b' ');
        line.push(b'\n');
        fs::write(&history, line).unwrap();

        let mut records = Vec::new();
        let mut malformed = 0;
        read_jsonl_records(&history, &mut records, &mut malformed).unwrap();

        assert_eq!(malformed, 0);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].get("n").and_then(Value::as_u64), Some(7));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_unterminated_history_line_is_skipped() {
        let dir = temp_dir("history-oversized-unterminated");
        let history = dir.join("battle-rewards.jsonl");
        let mut line = br#"{"record_type":"battle_reward","n":7}"#.to_vec();
        line.resize(MAX_JSONL_LINE_BYTES + 1, b' ');
        fs::write(&history, line).unwrap();

        let mut records = Vec::new();
        let mut malformed = 0;
        read_jsonl_records(&history, &mut records, &mut malformed).unwrap();

        assert_eq!(malformed, 1);
        assert!(records.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_records_do_not_rotate_and_corrupt_lines_are_skipped() {
        let dir = temp_dir("history-jsonl");
        let history = dir.join("battle-rewards.jsonl");
        let backup = history.with_extension("jsonl.1");
        fs::write(&backup, b"{\"record_type\":\"battle_reward\",\"n\":1}\n").unwrap();
        fs::write(
            &history,
            b"not-json\n{\"record_type\":\"battle_reward\",\"n\":2}\n",
        )
        .unwrap();
        let mut records = Vec::new();
        let mut malformed = 0;
        read_jsonl_records(&backup, &mut records, &mut malformed).unwrap();
        read_jsonl_records(&history, &mut records, &mut malformed).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(malformed, 1);
        // Cross the diagnostics rotation threshold, then append again: history
        // writes must not rotate the active file or discard the prior backup.
        let large_record =
            json!({"record_type":"battle_reward","payload":"x".repeat(5 * 1024 * 1024)});
        append_history_record(&history, &large_record).unwrap();
        assert!(fs::metadata(&history).unwrap().len() > 5 * 1024 * 1024);
        append_history_record(&history, &json!({"record_type":"battle_reward","n":3})).unwrap();
        assert!(backup.exists());
        assert!(fs::read_to_string(&backup).unwrap().contains("\"n\":1"));
        assert!(fs::metadata(&history).unwrap().len() > 5 * 1024 * 1024);
        assert_eq!(fs::read_to_string(&history).unwrap().lines().count(), 4);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn appending_after_torn_history_preserves_bytes_and_separates_new_record() {
        let dir = temp_dir("history-torn-append");
        let history = dir.join("battle-rewards.jsonl");
        let torn = b"{\"record_type\":\"incomplete\"";
        fs::write(&history, torn).unwrap();
        let record = json!({"record_type": "battle_reward", "run_id": "new-record"});

        append_history_record(&history, &record).unwrap();

        let mut expected = torn.to_vec();
        expected.push(b'\n');
        expected.extend(serde_json::to_vec(&record).unwrap());
        expected.push(b'\n');
        assert_eq!(fs::read(&history).unwrap(), expected);

        let mut records = Vec::new();
        let mut malformed = 0;
        read_jsonl_records(&history, &mut records, &mut malformed).unwrap();
        assert_eq!(malformed, 1);
        assert_eq!(records, vec![record]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracker_migration_separates_imports_from_torn_destination_line() {
        let dir = temp_dir("history-migration-torn-destination");
        let legacy = dir.join("legacy.jsonl");
        let destination = dir.join("Logs/battle-rewards.jsonl");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let torn = b"{\"record_type\":\"incomplete\"";
        let imported = json!({"record_type": "battle_reward", "run_id": "legacy-import"});
        fs::write(&destination, torn).unwrap();
        let mut legacy_bytes = serde_json::to_vec(&imported).unwrap();
        legacy_bytes.push(b'\n');
        fs::write(&legacy, legacy_bytes).unwrap();

        migrate_tracker_history(&legacy, &destination).unwrap();

        let mut expected = torn.to_vec();
        expected.push(b'\n');
        expected.extend(serde_json::to_vec(&imported).unwrap());
        expected.push(b'\n');
        assert_eq!(fs::read(&destination).unwrap(), expected);

        let mut records = Vec::new();
        let mut malformed = 0;
        read_jsonl_records(&destination, &mut records, &mut malformed).unwrap();
        assert_eq!(malformed, 1);
        assert_eq!(records, vec![imported]);
        assert!(legacy.with_extension("jsonl.migrated").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracker_history_migration_keeps_legacy_copy_and_deduplicates() {
        let dir = temp_dir("history-migration");
        let legacy = dir.join("legacy.jsonl");
        let destination = dir.join("Logs/battle-rewards.jsonl");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&legacy, b"same\nnew\n").unwrap();
        fs::write(&destination, b"same\n").unwrap();
        migrate_tracker_history(&legacy, &destination).unwrap();
        let recovery_copy = legacy.with_extension("jsonl.migrated");
        assert!(
            !legacy.exists(),
            "migrated legacy path should not be imported again"
        );
        assert!(
            recovery_copy.exists(),
            "legacy history must remain as a recovery copy"
        );
        assert_eq!(fs::read(&recovery_copy).unwrap(), b"same\nnew\n");
        assert_eq!(fs::read_to_string(&destination).unwrap().lines().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tracker_migration_with_existing_recovery_copy_is_idempotent() {
        let dir = temp_dir("history-migration-existing-recovery");
        let legacy = dir.join("legacy.jsonl");
        let destination = dir.join("Logs/battle-rewards.jsonl");
        let recovery_copy = legacy.with_extension("jsonl.migrated");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();

        let legacy_bytes = b"same\nnew\n";
        let recovery_bytes = b"older recovery snapshot\n";
        fs::write(&legacy, legacy_bytes).unwrap();
        fs::write(&destination, b"same\n").unwrap();
        fs::write(&recovery_copy, recovery_bytes).unwrap();

        migrate_tracker_history(&legacy, &destination).unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"same\nnew\n");
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        assert_eq!(fs::read(&recovery_copy).unwrap(), recovery_bytes);

        // A retry must neither duplicate imported lines nor replace either
        // recovery source when the migration marker already exists.
        migrate_tracker_history(&legacy, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"same\nnew\n");
        assert_eq!(fs::read(&legacy).unwrap(), legacy_bytes);
        assert_eq!(fs::read(&recovery_copy).unwrap(), recovery_bytes);

        fs::remove_dir_all(dir).unwrap();
    }

}
