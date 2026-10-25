# OpenCode Zen (`opencode-zen`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `opencode-zen`
- **Priority**: `205`
- **Primary Alias**: `ocz`
- **Additional Aliases**: `["opencode-zen"]`
- **UI Alias**: `ocz`
- **Display Name**: OpenCode Zen (Pay-As-You-Go + Free Tier)
- **Category**: `apikey`
- **Service Kinds**: `["llm", "systemone"]`
- **Website**: `https://opencode.ai/auth`
- **Executor**: `OpenCodeZenExecutor` (`open-sse/executors/opencode-zen.js`)
- **Source Files**:
  - `open-sse/providers/registry/opencode-zen.js`
  - `open-sse/executors/opencode-zen.js`
  - `open-sse/handlers/systemoneCore.js`

---

## 2. Endpoints by Format & Service Kind
1. **OpenAI Chat Completions (`transports[0]`)**:
   - `POST https://opencode.ai/zen/v1/chat/completions` (`Authorization: Bearer <apiKey>`)
2. **Claude / Anthropic Messages (`transports[1]`)**:
   - `POST https://opencode.ai/zen/v1/messages` (`x-api-key: <apiKey>`, `anthropic-version: 2023-06-01`)
3. **OpenAI Responses API (`transports[2]`)**:
   - `POST https://opencode.ai/zen/v1/responses` (`Authorization: Bearer <apiKey>`)
4. **System One Decision Endpoint (`systemoneConfig`)**:
   - `POST https://opencode.ai/zen/v1/systemone` (`x-opencode-client: desktop`, `User-Agent: opencode/1.18.31`, `x-opencode-session: <ses_...>`)
5. **Usage & Models Endpoints**:
   - `usage.url`: `https://opencode.ai/zen/v1/usage`
   - `modelsFetcher.url`: `https://opencode.ai/zen/v1/models` (`type: "opencode-free"`, `passthroughModels: true`)

---

## 3. Free-Tier Attestation, Session ID Encoding & Tool Fingerprinting (`OpenCodeZenExecutor`)
- **Bitwise-Inverted Session ID (`generateSessionId` / `translateSessionId`)**:
  - Format: `ses_<12hex_descending_time><14_base62>` matching `/^ses_[0-9a-f]{12}[0-9A-Za-z]{14}$/`.
  - The 12-hex prefix is `(~(BigInt(timeMs) * 0x1000n + BigInt(counter)) & ((1n << 48n) - 1n)).toString(16).padStart(12, "0")` with `counter = 1` (upstream decodes the prefix and rejects large counters with HTTP 403 `FreeTierError`).
- **Headers (`buildHeaders`)**:
  - `User-Agent`: `"opencode/1.18.31"` (unless downstream UA already has `opencode/>=1.17`)
  - `x-opencode-client`: `"desktop"`
  - `x-opencode-project`: `"global"`
  - `x-opencode-session`: `<ses_...>`
  - **Free-Tier Override (`isOpencodeFreeTierModel(model) === true`)**: Replaces `Authorization` with `"Bearer public"` and adds `x-opencode-request: msg_<12hex_time><14_base62>` (`generateRequestId()`), because sending a real `oc_sk_*` API key to a `-free` model returns HTTP 403 `FreeTierError`.
- **Mandatory Fingerprint Quartet (`OPENCODE_FINGERPRINT_TOOLS`)**:
  - Injects `["bash", "glob", "grep", "read"]` into `body.tools` (`ensureChatFingerprintTools` / `ensureResponsesFingerprintTools`) and forces `out.stream = true` on every request so upstream's anti-abuse filter does not reject with HTTP 403.

---

## 4. Supported Models (71+ Curated Models)
- **Claude (`supportedFormats: ["claude"]`)**: `claude-fable-5`, `claude-fable-5-1`, `claude-opus-5`, `claude-opus-4-8`, `claude-opus-4-7`, `claude-opus-4-6`, `claude-opus-4-5`, `claude-sonnet-5`, `claude-sonnet-4-6`, `claude-sonnet-4-5`, `claude-sonnet-4`, `claude-haiku-4-5`, `qwen3.6-plus`, `qwen3.5-plus`, `union-alpha`.
- **Responses (`supportedFormats: ["openai-responses"]`)**: `gpt-6-astra`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.5-pro`, `gpt-5.4`, `gpt-5.4-pro`, `gpt-5.4-mini`, `gpt-5.4-nano`, `gpt-5.3-codex-spark`, `gpt-5.3-codex`, `gpt-5.2`, `gpt-5.2-codex`, `gpt-5.1`, `gpt-5.1-codex-max`, `gpt-5.1-codex`, `gpt-5.1-codex-mini`, `gpt-5`, `gpt-5-codex`, `gpt-5-nano`, `grok-build-0.1`, `grok-4.6`, `grok-4.5`, `muse-spark-1.3`, `muse-spark-1.2`, `muse-spark-1.3-contributor-free`, `muse-spark-1.2-contributor-free`.
- **Chat Completions (`supportedFormats: ["openai"]`)**: `gemini-3.6-flash`, `gemini-3.8-flash`, `gemini-3.7-flash`, `gemini-3.5-flash-lite`, `gemini-3.5-flash`, `gemini-3.1-pro`, `gemini-3-flash`, `deepseek-v4-pro`, `deepseek-v4-flash`, `deepseek-v4-flash-vision-exp`, `glm-5.3-flash`, `glm-5.3`, `glm-5.2`, `glm-5.1`, `glm-5`, `minimax-m3`, `minimax-m2.7`, `minimax-m2.5`, `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.6`, `kimi-k2.5`, `big-pickle`, `deepseek-v4-flash-free`, `mimo-v2.6-flash-free`, `mimo-v2.5-free`, `ling-3.0-flash-fin-free`, `nemotron-3-ultra-free`, `nemotron-3.5-lightning-free`.
- **System One (`kind: "systemone"`)**: `jev-1.13`, `jev-1.13-free`.

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/opencode-zen.js` (Session Encoding & Headers Excerpt)
```javascript
const SESSION_PREFIX_MASK = (1n << 48n) - 1n;
const SESSION_TIME_MASK = (1n << 36n) - 1n;

function descendingTimeHex(time, counterValue) {
  return (~(BigInt(time) * 0x1000n + BigInt(counterValue)) & SESSION_PREFIX_MASK)
    .toString(16)
    .padStart(12, "0");
}

export function generateSessionId(timestamp = Date.now()) {
  return `ses_${descendingTimeHex(timestamp, 1)}${unstableRandom()}`;
}

export function generateRequestId(timestamp = Date.now()) {
  const current = BigInt(timestamp) * 0x1000n + 1n;
  const value = current;
  const time = Array.from({ length: 6 }, (_, index) =>
    Number((value >> BigInt(40 - 8 * index)) & 0xffn)
      .toString(16)
      .padStart(2, "0")
  ).join("");
  return `msg_${time}${unstableRandom()}`;
}
```
