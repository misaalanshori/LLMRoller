# MiMo Code Free (`mimo-free`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `mimo-free`
- **Priority**: `50` (`hasFree: true`)
- **Alias / UI Alias**: `mmf`
- **Hidden**: `true`
- **Display Name**: MiMo Code Free (Xiaomi)
- **Category**: `free` (`noAuth: true`)
- **Executor**: `MimoFreeExecutor` (`open-sse/executors/mimo-free.js`)
- **Source Files**:
  - `open-sse/providers/registry/mimo-free.js`
  - `open-sse/executors/mimo-free.js`

---

## 2. Endpoints & Device-Fingerprinted JWT Bootstrap
1. **Bootstrap Endpoint (`bootstrapJwt`)**:
   - `POST https://api.xiaomimimo.com/api/free-ai/bootstrap`
   - Headers: `Content-Type: application/json`, `User-Agent: <random Chrome UA>`
   - Body: `{ "client": "<sha256_machine_fingerprint>" }` where the fingerprint hashes `${os.hostname()}|${os.platform()}|${os.arch()}|${cpu}|${username}`.
   - Returns `{ "jwt": "<token>" }`. Cached in memory until 5 minutes before the JWT's `exp` claim (or 3000s fallback TTL).
2. **Chat Endpoint**:
   - `POST https://api.xiaomimimo.com/api/free-ai/openai/chat`
   - On HTTP `401` or `403`, invalidates the cached JWT, re-runs `bootstrapJwt()`, and retries once.
3. **Models Fetcher**:
   - `GET https://models.dev/api.json` (`type: "mimo-free"`, `passthroughModels: true`)

---

## 3. Headers & Anti-Abuse System Marker (`MimoFreeExecutor`)
- **Request Headers**:
  - `Authorization`: `Bearer <bootstrapped_jwt>`
  - `Content-Type`: `"application/json"`
  - `X-Mimo-Source`: `"mimocode-cli-free"`
  - `User-Agent`: Randomly chosen Chrome 131 macOS / Windows / Linux User-Agent (upstream returns HTTP 403 `"Illegal access"` without a Chrome User-Agent).
  - `x-session-affinity`: `ses_<24_random_alphanumeric_chars>`
  - `Accept`: `"text/event-stream"` or `"application/json"`
- **Mandatory System Marker (`injectSystemMarker`)**:
  - Upstream returns HTTP 403 `"Illegal access"` unless a `role: "system"` message contains:
    `"You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks."`
  - `transformRequest` automatically prepends `{ role: "system", content: MIMO_SYSTEM_MARKER }` if no existing system message contains that substring.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `mimo-auto` | MiMo Auto |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/mimo-free.js` (Excerpt)
```javascript
const BOOTSTRAP_URL = "https://api.xiaomimimo.com/api/free-ai/bootstrap";
const CHAT_URL = PROVIDERS["mimo-free"].baseUrl;
export const MIMO_SYSTEM_MARKER =
  "You are MiMoCode, an interactive CLI tool that helps users with software engineering tasks.";

function generateFingerprint() {
  let username = "unknown-user";
  try {
    username = os.userInfo().username;
  } catch {}
  const cpu = (os.cpus()[0]?.model || "unknown-cpu").trim();
  const seed = `${os.hostname()}|${os.platform()}|${os.arch()}|${cpu}|${username}`;
  return createHash("sha256").update(seed).digest("hex");
}

function injectSystemMarker(body) {
  const messages = body?.messages;
  if (!Array.isArray(messages)) return body;
  const hasMarker = messages.some(
    (m) => m?.role === "system" && typeof m.content === "string" && m.content.includes(MIMO_SYSTEM_MARKER)
  );
  if (hasMarker) return body;
  return { ...body, messages: [{ role: "system", content: MIMO_SYSTEM_MARKER }, ...messages] };
}
```
