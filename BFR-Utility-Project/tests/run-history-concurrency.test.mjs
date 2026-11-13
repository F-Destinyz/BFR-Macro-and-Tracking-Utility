import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const trackerHtml = fs.readFileSync(path.join(root, 'dist', 'tracker.html'), 'utf8');
const script = [...trackerHtml.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/gi)].map((match) => match[1]).at(-1);

assert.ok(script, 'Run History dashboard script was not found');

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function element() {
  const listeners = new Map();
  return {
    disabled: false,
    value: '',
    title: '',
    textContent: '',
    innerHTML: '',
    className: '',
    listeners,
    attributes: new Map(),
    classList: { toggle() {} },
    addEventListener(name, callback) {
      listeners.set(name, callback);
    },
    setAttribute(name, value) {
      this.attributes.set(name, value);
    },
    removeAttribute(name) {
      this.attributes.delete(name);
    }
  };
}

function createHarness({ pages, clearResult }) {
  const elements = new Map();
  const calls = [];
  const confirmations = [];
  const getElementById = (id) => {
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  };
  const document = { getElementById };
  const window = {
    confirm(message) {
      confirmations.push(message);
      return true;
    },
    __TAURI_INTERNALS__: {
      invoke(command, payload) {
        calls.push({ command, payload });
        if (command === 'tracker_get_pinned') return Promise.resolve(false);
        if (command === 'tracker_set_pinned') return Promise.resolve();
        if (command === 'tracker_read_data_page') {
          const page = pages.shift();
          return page === undefined ? Promise.resolve({ records: [], total_records: 0, next_offset: 0, has_more: false }) : page;
        }
        if (command === 'tracker_clear_history') return clearResult();
        throw new Error(`Unexpected tracker command: ${command}`);
      }
    }
  };

  vm.runInNewContext(script, { window, document, console: { error() {}, warn() {} } });
  return { elements, calls, confirmations, getElementById };
}

function callsFor(harness, command) {
  return harness.calls.filter((call) => call.command === command);
}

function nextTask() {
  return new Promise((resolve) => setImmediate(resolve));
}

const emptyPage = { records: [], total_records: 0, next_offset: 0, has_more: false };

test('Run History reads and clear remain mutually exclusive during overlapping UI actions', async () => {
  const pendingRead = deferred();
  const pendingClear = deferred();
  const harness = createHarness({
    pages: [pendingRead.promise],
    clearResult: () => pendingClear.promise
  });

  assert.equal(callsFor(harness, 'tracker_read_data_page').length, 1,
    'Initial dashboard load should issue one page read');

  await harness.getElementById('clearHistory').listeners.get('click')();
  assert.equal(callsFor(harness, 'tracker_clear_history').length, 0,
    'Clear must not start while a page read is active');
  assert.equal(harness.confirmations.length, 0,
    'A blocked clear must not show a confirmation prompt');

  pendingRead.resolve({ ...emptyPage, has_more: true });
  await nextTask();

  const clearButton = harness.getElementById('clearHistory');
  const refreshButton = harness.getElementById('refresh');
  const loadOlderButton = harness.getElementById('loadOlder');
  const clearTask = clearButton.listeners.get('click')();

  assert.equal(callsFor(harness, 'tracker_clear_history').length, 1,
    'A clear should start once the page read has completed');
  assert.equal(clearButton.disabled, true);
  assert.equal(refreshButton.disabled, true);
  assert.equal(loadOlderButton.disabled, true);

  await refreshButton.listeners.get('click')();
  await loadOlderButton.listeners.get('click')();
  await clearButton.listeners.get('click')();

  assert.equal(callsFor(harness, 'tracker_read_data_page').length, 1,
    'Refresh and Load Older must not start reads while clearing');
  assert.equal(callsFor(harness, 'tracker_clear_history').length, 1,
    'A second clear must not overlap the active clear');

  pendingClear.resolve();
  await clearTask;

  assert.match(harness.getElementById('status').textContent, /Run History cleared/);
  assert.equal(clearButton.disabled, false);
  assert.equal(refreshButton.disabled, false);
  assert.equal(loadOlderButton.disabled, true);
});

test('a failed Run History clear reloads disk records before showing the clear error', async () => {
  const pendingClear = deferred();
  const recoveredRecord = {
    record_type: 'battle_reward',
    timestamp_unix_ms: 1000,
    dungeon_name: 'Recovered after partial clear',
    path: '/api/battles/finish',
    run_count: 1,
    totals: { zel: 12 }
  };
  const harness = createHarness({
    pages: [
      Promise.resolve(emptyPage),
      Promise.resolve({ ...emptyPage, records: [recoveredRecord], total_records: 1 })
    ],
    clearResult: () => pendingClear.promise
  });

  await nextTask();
  const clearTask = harness.getElementById('clearHistory').listeners.get('click')();
  pendingClear.reject(new Error('simulated clear failure'));
  await clearTask;

  assert.equal(callsFor(harness, 'tracker_read_data_page').length, 2,
    'Clear failure must trigger a fresh read from disk');
  assert.match(harness.getElementById('historyTable').innerHTML, /Recovered after partial clear/,
    'The dashboard must render the recovered disk state');
  assert.match(harness.getElementById('status').textContent, /simulated clear failure/,
    'The original clear failure must remain visible after recovery');
});

test('Refresh reloads the newest page instead of treating the click event as Load Older', async () => {
  const latest = {
    record_type: 'battle_reward',
    timestamp_unix_ms: 2000,
    dungeon_name: 'Latest before refresh',
    path: '/api/battles/finish',
    run_count: 1,
    totals: { zel: 20 }
  };
  const older = {
    record_type: 'battle_reward',
    timestamp_unix_ms: 1000,
    dungeon_name: 'Older loaded record',
    path: '/api/battles/finish',
    run_count: 1,
    totals: { zel: 10 }
  };
  const refreshed = {
    record_type: 'battle_reward',
    timestamp_unix_ms: 3000,
    dungeon_name: 'Newest after refresh',
    path: '/api/battles/finish',
    run_count: 1,
    totals: { zel: 30 }
  };
  const harness = createHarness({
    pages: [
      Promise.resolve({ records: [latest], total_records: 2, next_offset: 1, has_more: true }),
      Promise.resolve({ records: [older], total_records: 2, next_offset: 2, has_more: false }),
      Promise.resolve({ records: [refreshed], total_records: 3, next_offset: 1, has_more: true })
    ],
    clearResult: () => Promise.resolve()
  });

  await nextTask();
  assert.match(harness.getElementById('historyTable').innerHTML, /Latest before refresh/);
  await harness.getElementById('loadOlder').listeners.get('click')();
  assert.match(harness.getElementById('historyTable').innerHTML, /Older loaded record/);

  const search = harness.getElementById('historySearch');
  search.value = 'Older loaded record';
  search.listeners.get('input')();
  assert.match(harness.getElementById('historyTable').innerHTML, /Older loaded record/);
  assert.doesNotMatch(harness.getElementById('historyTable').innerHTML, /Latest before refresh/);
  search.value = '';
  search.listeners.get('input')();

  // Real browser click listeners receive a MouseEvent argument. It must not be
  // mistaken for the load(append=true) flag.
  await harness.getElementById('refresh').listeners.get('click')({ type: 'click' });

  const reads = callsFor(harness, 'tracker_read_data_page');
  assert.equal(reads.length, 3);
  assert.deepEqual({ ...reads[2].payload }, { offset: 0, anchorTotal: 0, limit: 100 });
  assert.match(harness.getElementById('historyTable').innerHTML, /Newest after refresh/);
  assert.doesNotMatch(harness.getElementById('historyTable').innerHTML, /Older loaded record/);
});
