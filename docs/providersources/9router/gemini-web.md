# Gemini Web (`gemini-web`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `gemini-web`
- **Priority**: `220`
- **Primary Alias**: `gemini-web`
- **Additional Aliases**: `["gweb"]`
- **UI Alias**: `gemini-web`
- **Display Name**: Gemini Web
- **Category**: `webCookie`
- **Auth Type**: `cookie` (`__Secure-1PSID` and `__Secure-1PSIDTS` from `gemini.google.com`)
- **Service Kinds**: `["image"]` (LLM chat turns are also implemented in `GeminiWebExecutor` via Playwright DOM automation)
- **Website**: `https://gemini.google.com`
- **Executor**: `GeminiWebExecutor` (`open-sse/executors/gemini-web.js` and `open-sse/executors/gemini-web/*.js`)
- **Source Files**:
  - `open-sse/providers/registry/gemini-web.js`
  - `open-sse/executors/gemini-web.js`
  - `open-sse/executors/gemini-web/browserLease.js`
  - `open-sse/executors/gemini-web/capabilities.js`
  - `open-sse/executors/gemini-web/domCapture.js`
  - `open-sse/executors/gemini-web/imageGen.js`
  - `open-sse/executors/gemini-web/modeSelection.js`

---

## 2. Playwright Browser Automation & DOM Capture Architecture
- **Target Page**: `https://gemini.google.com/app`
- **Browser Pool (`browserLease.js`)**: Maintains a warm headless Chromium process (`GEMINI_WEB_BROWSER_IDLE_MS = 60000`), creating an isolated `browser.newContext({ userAgent })` per request and injecting `.google.com` cookies via `toPlaywrightCookies(cookie, ".google.com")`.
- **Quota & Blocker Detection (`watchGeminiQuota` & `geminiAppBlocker`)**:
  - Watches background XHR responses matching `/\/BardChatUi\/data\/batchexecute/` for `/used all of your requests|quota will reset|quota exceeded|reached your limit/i`.
  - Detects marketing-page redirects or `"Something went wrong"` screens before waiting for `.ql-editor, [contenteditable='true']`.
- **Fail-Closed UI Mode Selection (`modeSelection.js`)**:
  - `gemini-3.1-pro` is treated as the default landing mode (`isDefault: true`).
  - For `gemini-3.7-flash`, `gemini-3.1-flash-lite`, or `Extended Thinking`, clicks the mode menu/toggle (`[data-test-id="bard-mode-menu-button"]` or `[data-test-id="deep-think-toggle"]`) and reads back the indicator text; if unconfirmed, rejects with HTTP 400 (`unsupported_control_for_provider`).
- **DOM Reply Capture (`domCapture.js`)**:
  - Polls `document.querySelectorAll("model-response, message-content")` every `400ms` while checking `[data-test-id="thinking-overlay-content"], button[aria-label*="Stop" i]`.
  - Emits text deltas once the thinking overlay clears and completes after `STABLE_POLLS_DONE = 5` unchanged polls.
- **Image Generation (`imageGen.js`)**:
  - Types `Generate an image (aspect ratio <size>): <prompt>` into the composer, waits for `<img>` elements (`naturalWidth >= 100`, non-`gstatic`) inside the latest `model-response`, and extracts Base64 PNG bytes via an in-page `<canvas>` or `fetch()`.

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `gemini-3.1-pro` | Gemini 3.1 Pro | `llm` | — |
| `gemini-3.7-flash` | Gemini 3.7 Flash | `llm` | — |
| `gemini-3.1-flash-lite` | Gemini 3.1 Flash-Lite | `llm` | — |
| `gemini-image` | Gemini Image (web) | `image` | `["n", "size"]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/gemini-web.js`
```javascript
export default {
  id: "gemini-web",
  priority: 220,
  alias: "gemini-web",
  aliases: [
    "gweb",
  ],
  uiAlias: "gemini-web",
  display: {
    name: "Gemini Web",
    icon: "auto_awesome",
    color: "#4285F4",
    textIcon: "GW",
    website: "https://gemini.google.com",
  },
  category: "webCookie",
  authType: "cookie",
  serviceKinds: ["image"],
  authHint: "Paste your __Secure-1PSID (and __Secure-1PSIDTS) cookie value(s) from gemini.google.com",
  transport: {
    baseUrl: "https://gemini.google.com/app",
    format: "openai",
    authType: "cookie",
  },
  models: [
    { id: "gemini-3.1-pro", name: "Gemini 3.1 Pro" },
    { id: "gemini-3.7-flash", name: "Gemini 3.7 Flash" },
    { id: "gemini-3.1-flash-lite", name: "Gemini 3.1 Flash-Lite" },
    { id: "gemini-image", name: "Gemini Image (web)", params: ["n", "size"], kind: "image" },
  ],
};
```

### `open-sse/executors/gemini-web/domCapture.js` (Excerpt)
```javascript
const ASSISTANT_SELECTOR = "model-response, message-content";

const READ_REPLY_STATE = `(() => {
  const msgs = [...document.querySelectorAll(${JSON.stringify(ASSISTANT_SELECTOR)})];
  const last = msgs.length ? (msgs[msgs.length - 1].innerText || '').trim() : '';
  return {
    count: msgs.length,
    lastText: last,
    generating: !!document.querySelector('[data-test-id="thinking-overlay-content"], button[aria-label*="Stop" i]'),
  };
})()`;
```
