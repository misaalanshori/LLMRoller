# OpenCode Free (`opencode`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `opencode`
- **Priority**: `40` (`hasFree: true`)
- **Alias / UI Alias**: `oc`
- **Display Name**: OpenCode Free
- **Category**: `free` (`noAuth: true`)
- **Service Kinds**: `["llm", "systemone"]`
- **Executor**: `OpenCodeExecutor` (`open-sse/executors/opencode.js`)
- **Source Files**:
  - `open-sse/providers/registry/opencode.js`
  - `open-sse/executors/opencode.js`
  - `open-sse/utils/opencodeFingerprint.js`

---

## 2. OpenCode v2 Free-Tier Endpoints (`buildUrl`)
OpenCode v2 moved its free tier from `/zen/v1` to `/inference/v1`. `OpenCodeExecutor.buildUrl` routes based on the model:
1. **Responses API (`isResponsesModel(model) === true`)**:
   - `POST https://opencode.ai/inference/v1/responses` (serves `muse-spark-1.2-contributor-free`, `muse-spark-1.3-contributor-free`, and all Muse Spark models)
2. **Claude Messages API (`isMessagesModel(model) === true`)**:
   - `POST https://opencode.ai/inference/v1/messages` (serves `union-alpha`)
3. **OpenAI Chat Completions API (Default for all other free models)**:
   - `POST https://opencode.ai/inference/v1/chat/completions`
4. **System One Decision Endpoint (`systemoneConfig`)**:
   - `POST https://opencode.ai/zen/v1/systemone` (serves `jev-1.13-free`)
5. **Models Catalog Fetcher**:
   - `GET https://opencode.ai/zen/v1/models` (`type: "opencode-free"`, `passthroughModels: true`)

---

## 3. OpenCode v2 Attestation Headers & Tool Cloaking (`OpenCodeExecutor`)
- **OpenCode v2 CLI Headers (`buildHeaders`)**:
  - `Content-Type`: `"application/json"`
  - `Authorization`: `"Bearer public"`
  - `User-Agent`: `"opencode/latest/2.0.26/cli"` (upstream rejects v1 `"opencode/1.18.x"` strings on `/inference/v1` with HTTP 403 `FreeTierError`)
  - `x-opencode-client`: `"cli"`
  - `x-opencode-session`: `<ses_12hex_14base62>` (reused per downstream identity via `stableOpencodeSessions` map to avoid triggering per-session `429 FreeUsageLimitError`)
  - `x-opencode-session-id`: `<ses_12hex_14base62>` (required alongside `x-opencode-session` in v2; `x-opencode-request` is omitted because v2 rejects it with 403)
  - `x-session-affinity`: `<ses_12hex_14base62>`
  - `x-session-id`: `<ses_12hex_14base62>`
  - `x-opencode-project`: `"global"`
  - `anthropic-version`: `"2023-06-01"` (when `url.endsWith("/messages")`)
- **Request Transformations**:
  - Forces `body.stream = true`.
  - Runs `applyFingerprintTools(body, isResponses)` to inject the required OpenCode built-in tools (`bash`, `glob`, `grep`, `read`).
  - Runs `injectReasoningContent({ provider: "opencode", model, body })` (`reasoningInject: { scope: "all" }`).
  - Sets `quirks.forceAutoToolChoiceModels: ["muse-spark-1.3-contributor-free"]` and `quirks.reasoningAliasFields: true`.

---

## 4. Verbatim Source Code Snippets

### `open-sse/executors/opencode.js` (`buildUrl` & `buildHeaders`)
```javascript
const OPENCODE_UA = "opencode/latest/2.0.26/cli";
const OPENCODE_CLIENT = "cli";

  buildUrl(model) {
    const base = this.config.baseUrl;
    if (isResponsesModel(model)) return `${base}/inference/v1/responses`;
    if (isMessagesModel(model)) return `${base}/inference/v1/messages`;
    return `${base}/inference/v1/chat/completions`;
  }

  buildHeaders(credentials, stream = true, url = "") {
    const raw = credentials?.rawHeaders || {};
    const lower = {};
    for (const [k, v] of Object.entries(raw)) lower[k.toLowerCase()] = v;

    const downstreamUa = lower["user-agent"] || "";
    const isOpencodeDownstream = hasValidOpencodeVersion(downstreamUa);

    const session = credentials?.[SESSION_FIELD] || this.prepareRequestCredentials({ credentials })[SESSION_FIELD];

    const headers = {
      "Content-Type": "application/json",
      "Authorization": "Bearer public",
      "User-Agent": isOpencodeDownstream ? downstreamUa : OPENCODE_UA,
      "x-opencode-client": lower["x-opencode-client"] || OPENCODE_CLIENT,
      "x-opencode-session": session,
      "x-opencode-session-id": session,
      "x-session-affinity": session,
      "x-session-id": session,
      "x-opencode-project": lower["x-opencode-project"] || "global",
      "Accept": stream ? "text/event-stream" : "*/*",
    };
    if (url.endsWith("/messages")) headers["anthropic-version"] = ANTHROPIC_API_VERSION;
    return headers;
  }
```
