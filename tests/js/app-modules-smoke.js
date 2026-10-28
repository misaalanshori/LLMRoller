#!/usr/bin/env node
// Load-order and dispatch smoke check for the split `app-*.js` modules (ISSUE-0013).
//
// There is no JS unit harness in this repo, so this is deliberately not one: it is a single
// executable assertion that the twelve modules, loaded in the order the templates use, share
// one global scope without a ReferenceError, and that the delegated `click` dispatcher still
// reaches its handlers. Those are the two ways a split breaks a page silently — a load-order
// temporal-dead-zone error, or a dispatcher calling a function that moved out of scope.
//
// Run: `node tests/js/app-modules-smoke.js` (needs Node only, no `npm install`).
//
// Wired into `cargo test` by `static_asset_modules_load_in_order_and_dispatch` in
// `crates/server/tests/workspace_boundaries.rs`, which shells out to this file, so the check
// cannot rot by being forgotten. If Node is unavailable the test skips with a message.

const fs = require('fs');
const vm = require('vm');
const path = require('path');

// Must match APP_JS_MODULES in crates/admin/src/assets.rs and the <script> order in the
// templates. The Rust-side test asserts the two agree.
const ORDER = [
  'core', 'providers', 'keys', 'catalog', 'settings', 'charts',
  'prices', 'sessions', 'accounts', 'shares', 'users', 'auth',
];

// Every function the delegated listeners invoke by name. Derived from the listener bodies in
// app-core.js; a name that moved out of the shared scope fails here.
const REQUIRED = [
  'switchTab', 'refreshPartial', 'invalidateAndRefetch', 'openModal', 'closeModal',
  'copySnippet', 'escapeHtml', 'previewText', 'getCsrfToken', 'onProviderKindChange',
  'acceptShare', 'applyDataPlaneOrigin', 'confirmModelMerge', 'deleteAlias',
  'deletePriceEntry', 'deleteServiceAccount', 'handleAddAlias', 'handleChangePassword',
  'handleCreateClientKey', 'handleCreateGlobalProvider', 'handleCreateProvider',
  'handleCreateServiceAccount', 'handleCreateShare', 'handleCreateUpstreamKey',
  'handleCreateUser', 'handleEnrollTotp', 'handleLoginSubmit', 'handleSaveBudgetWindow',
  'handleSavePriceEntry', 'handleSaveTraceabilityConfig', 'handleSaveUserPreferences',
  'handleSetupSubmit', 'handleVerifyTotp', 'initConnectionSetupUrls', 'loadAdminAliases',
  'loadAdminPrices', 'loadAdminSettings', 'loadPersonalOverviewAndChart',
  'loadServiceAccounts', 'loadUsageChart', 'loadWebSessions', 'openChatPreview',
  'populateShareProviders', 'revokeWebSession', 'rotateClientKey', 'saveSetting',
  'selectApiFormat', 'showRevealedTokenModal', 'syncUserPreferenceInputs', 'testUpstreamKey',
  'toggleClientKeyPause', 'toggleModelExposed', 'toggleUser',
];

const listeners = [];
const noop = () => {};
const fakeEl = () => ({
  classList: { add: noop, remove: noop, contains: () => false },
  querySelectorAll: () => [], querySelector: () => null,
  addEventListener: noop, setAttribute: noop, getAttribute: () => null,
  innerHTML: '', value: '', style: {}, appendChild: noop, closest: () => null,
  getContext: () => null, clientWidth: 640,
});

const sandbox = {
  document: {
    addEventListener: (t, f) => listeners.push([t, f]),
    querySelector: () => null, querySelectorAll: () => [],
    getElementById: () => null, createElement: fakeEl,
    body: fakeEl(), documentElement: fakeEl(),
  },
  window: { addEventListener: (t, f) => listeners.push([t, f]) },
  navigator: { clipboard: { writeText: noop } },
  alert: noop,
  fetch: () => Promise.resolve({ ok: true, text: () => Promise.resolve('') }),
  console, setTimeout, JSON, Math, Date, parseInt, parseFloat, String, Number,
  Array, Object, Promise, Set, Map, RegExp, Error, isNaN,
  encodeURIComponent, decodeURIComponent,
  htmx: { config: {}, process: noop },
  uPlot: function () { return { destroy: noop, setSize: noop }; },
};
sandbox.globalThis = sandbox;
const ctx = vm.createContext(sandbox);

const staticDir = path.join(__dirname, '..', '..', 'crates', 'admin', 'static');

for (const module of ORDER) {
  const file = path.join(staticDir, `app-${module}.js`);
  const src = fs.readFileSync(file, 'utf8');
  try {
    vm.runInContext(src, ctx, { filename: `app-${module}.js` });
  } catch (error) {
    console.error(`LOAD FAILURE in app-${module}.js: ${error.name}: ${error.message}`);
    process.exit(1);
  }
}

const missing = REQUIRED.filter((name) => typeof ctx[name] !== 'function');
if (missing.length) {
  console.error(`MISSING GLOBALS after loading all modules: ${missing.join(', ')}`);
  process.exit(1);
}

// --- Dispatch: a synthetic click must reach the handler for its `data-*` attribute -------
let tabCalled = null;
ctx.switchTab = (id) => { tabCalled = id; };
let modalOpened = null;
ctx.openModal = (id) => { modalOpened = id; };

function fakeTarget(attrs) {
  const stub = (matched) => ({
    getAttribute: (key) => (key in attrs ? attrs[key] : null),
    closest: () => null,
    classList: { add: noop, remove: noop },
    querySelectorAll: () => [],
    ...matched,
  });
  return {
    closest: (selector) => {
      const bare = selector.match(/^\[([a-z-]+)\]$/);
      if (bare && attrs[bare[1]] !== undefined) return stub({});
      const valued = selector.match(/^\[([a-z-]+)="([^"]*)"\]$/);
      if (valued && attrs[valued[1]] === valued[2]) return stub({});
      return null;
    },
  };
}

const clickHandler = listeners.find(([type]) => type === 'click');
if (!clickHandler) {
  console.error('DISPATCH FAILURE: no click listener was registered');
  process.exit(1);
}

clickHandler[1]({ target: fakeTarget({ 'data-tab': 'tab-overview' }) });
if (tabCalled !== 'tab-overview') {
  console.error(`DISPATCH FAILURE: click [data-tab] did not reach switchTab (got ${tabCalled})`);
  process.exit(1);
}

clickHandler[1]({ target: fakeTarget({ 'data-modal-open': 'modal-new-key' }) });
if (modalOpened !== 'modal-new-key') {
  console.error(`DISPATCH FAILURE: click [data-modal-open] did not reach openModal (got ${modalOpened})`);
  process.exit(1);
}

console.log(
  `OK: ${ORDER.length} modules loaded in order, ${REQUIRED.length} listener-reachable ` +
  `functions defined, delegated click dispatch reaches switchTab and openModal`
);
