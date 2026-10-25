# ChatGPT Web (`chatgpt-web`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `chatgpt-web`
- **Priority**: `220`
- **Primary Alias**: `chatgpt-web`
- **Additional Aliases**: `["cg"]`
- **UI Alias**: `chatgpt-web`
- **Display Name**: ChatGPT Web
- **Category**: `webCookie`
- **Auth Type**: `cookie`
- **Service Kinds**: `["image"]` (LLM + tools are also fully implemented over both plain HTTP and Playwright browser session, though registry currently sets `serviceKinds: ["image"]`)
- **Website**: `https://chatgpt.com`
- **Executor**: `ChatGptWebExecutor` (`open-sse/executors/chatgpt-web.js` and `open-sse/executors/chatgpt-web/*.js`)
- **Source Files**:
  - `open-sse/providers/registry/chatgpt-web.js`
  - `open-sse/executors/chatgpt-web.js`
  - `open-sse/executors/chatgpt-web/adapter.js`
  - `open-sse/executors/chatgpt-web/attachments.js`
  - `open-sse/executors/chatgpt-web/browserSession.js`
  - `open-sse/executors/chatgpt-web/chatHttp.js`
  - `open-sse/executors/chatgpt-web/deltaV1.js`
  - `open-sse/executors/chatgpt-web/firstParty.js`
  - `open-sse/executors/chatgpt-web/imageGen.js`
  - `open-sse/executors/chatgpt-web/imageGenHttp.js`
  - `open-sse/executors/chatgpt-web/imageSniff.js`
  - `open-sse/executors/chatgpt-web/sentinel.js`

---

## 2. Endpoints & Dual-Path Transport (Plain HTTP + Playwright Fallback)
`ChatGptWebExecutor` implements a dual-path execution engine: it attempts a pure Node HTTP turn first (solving Sentinel proof-of-work in pure JS via `sentinel.js`), and falls back to an in-page Playwright browser turn (`https://chatgpt.com/?temporary-chat=true`) only if Cloudflare/OpenAI enforces a Turnstile VM challenge (`TURNSTILE_REQUIRED`).

### Key Upstream Endpoints (`https://chatgpt.com`)
1. **Session Token Exchange**:
   - `GET https://chatgpt.com/api/auth/session`
   - Headers: `Cookie: <cookie>`, `User-Agent: <ua>`, `Accept: application/json`
   - Returns `{ accessToken: "<jwt>", ... }`
2. **Sentinel Requirements Prepare**:
   - `POST https://chatgpt.com/backend-api/sentinel/chat-requirements/prepare`
   - Headers: `Authorization: Bearer <accessToken>`, `OpenAI-Sentinel-Chat-Requirements-Prepare-Token: gAAAAAC<base64_24_slot_fingerprint>`
   - Returns `{ prepare_token: "..." }`
3. **Sentinel Requirements & Proof-of-Work**:
   - `POST https://chatgpt.com/backend-api/sentinel/chat-requirements`
   - Headers: `Authorization: Bearer <accessToken>`, `OpenAI-Sentinel-Chat-Requirements-Prepare-Token: <prepare_token>`
   - Returns `{ token, proofofwork: { required, seed, difficulty }, turnstile: { ... } }`
4. **Conduit Prepare**:
   - `POST https://chatgpt.com/backend-api/f/conversation/prepare`
   - Headers: Sentinel headers + `Authorization: Bearer <accessToken>`, `Cookie`, `Accept: application/json`
   - Returns `{ conduit_token: "..." }`
5. **Conversation Stream (Chat & Picture v2)**:
   - `POST https://chatgpt.com/backend-api/f/conversation`
   - Headers: Sentinel headers + `Authorization: Bearer <accessToken>`, `X-Conduit-Token: <conduit_token>`, `X-Oai-Turn-Trace-Id: <uuid>`, `Accept: text/event-stream`
   - Returns SSE stream using `delta_encoding: v1` (JSON Pointer `add`/`append`/`patch`/`replace` operations decoded by `ChatGptWebDeltaV1Decoder`).
6. **Conversation Document Poll**:
   - `GET https://chatgpt.com/backend-api/conversation/<conversationId>`
   - Polled until assistant message `metadata.is_complete === true` (for chat) or `file-service://` / `sediment://` asset pointers appear (for `picture_v2` image generation).
7. **Attachment Registration & Upload (Browser Path)**:
   - `POST /backend-api/files` -> returns `{ file_id, upload_url }`
   - `PUT <upload_url>` (on `*.oaiusercontent.com` with `x-ms-blob-type: BlockBlob`, `x-ms-version: 2020-04-08`)
   - `POST /backend-api/files/process_upload_stream`
8. **Generated Image Download**:
   - `GET https://chatgpt.com/backend-api/files/<fileId>/download`
   - `GET https://chatgpt.com/backend-api/conversation/<conversationId>/attachment/<sedimentId>/download`

---

## 3. Authentication & Sentinel PoW Algorithm
- **Input Credentials**: Accepts either:
  - A full Playwright `storageState` JSON (`{ cookies: [...], origins: [...] }`) restricted to first-party domains (`chatgpt.com`, `openai.com`), normalizing `__Host-` cookies to strip leading dots.
  - A Cookie header containing `__Secure-next-auth.session-token=...`
  - A bare `__Secure-next-auth.session-token` JWE string.
- **Sentinel Hash (`sentinelHash`)**:
  - 32-bit FNV-1a (`0x811c9dc5`, prime `0x1000193`) followed by 3 MurmurHash3-style avalanche rounds (`0x85ebca6b`, `0xc2b2ae35`), formatted as 8 lowercase hex characters.
  - Brute-forces `slots[3] = counter` up to `500,000` iterations until `sentinelHash(seed + base64(JSON.stringify(slots))).slice(0, difficulty.length) <= difficulty`, emitting `OpenAI-Sentinel-Proof-Token: gAAAAAB<base64>~S`.

---

## 4. Supported Models & UI Picker Resolution
| Model ID | Display Name | Selection Mapping (`resolveSelection`) |
|---|---|---|
| `gpt-5-6` | GPT-5.6 Sol — Instant | `{ kind: "picker", modelLabel: "GPT-5.6 Sol", effortIndex: 0 }` |
| `gpt-5-6-thinking` | GPT-5.6 Sol — Thinking | `{ kind: "picker", modelLabel: "GPT-5.6 Sol", effortIndex: <0..3> }` |
| `gpt-5-6-pro` | GPT-5.6 Sol — Pro | `{ kind: "picker", modelLabel: "GPT-5.6 Sol", effortIndex: 4 }` |
| `gpt-5.6-luna-free` | GPT-5.6 Luna — Free | `{ kind: "free", thinkEnabled: false }` |
| `gpt-5.6-luna-free-thinking` | GPT-5.6 Luna — Free Thinking | `{ kind: "free", thinkEnabled: true }` |
| `gpt-5-5-instant` | GPT-5.5 — Instant | `{ kind: "picker", modelLabel: "GPT-5.5", effortIndex: 0 }` |
| `gpt-5-5-thinking` | GPT-5.5 — Thinking | `{ kind: "picker", modelLabel: "GPT-5.5", effortIndex: <0..3> }` |
| `gpt-5-5-pro` | GPT-5.5 — Pro | `{ kind: "picker", modelLabel: "GPT-5.5", effortIndex: 4 }` |
| `gpt-image-2` | GPT Image 2 (web) | `kind: "image"`, `params: ["n", "size"]` (`system_hints: ["picture_v2"]`) |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/chatgpt-web.js`
```javascript
export default {
  id: "chatgpt-web",
  priority: 220,
  alias: "chatgpt-web",
  aliases: [
    "cg",
  ],
  uiAlias: "chatgpt-web",
  display: {
    name: "ChatGPT Web",
    icon: "auto_awesome",
    color: "#10A37F",
    textIcon: "CG",
    website: "https://chatgpt.com",
  },
  category: "webCookie",
  authType: "cookie",
  serviceKinds: ["image"],
  authHint: "Paste your chatgpt.com cookie header containing __Secure-next-auth.session-token (or a full Playwright storage-state JSON)",
  transport: {
    baseUrl: "https://chatgpt.com",
    format: "openai",
    authType: "cookie",
  },
  models: [
    { id: "gpt-5-6", name: "GPT-5.6 Sol — Instant" },
    { id: "gpt-5-6-thinking", name: "GPT-5.6 Sol — Thinking", supportsReasoning: true },
    { id: "gpt-5-6-pro", name: "GPT-5.6 Sol — Pro", supportsReasoning: true },
    { id: "gpt-5.6-luna-free", name: "GPT-5.6 Luna — Free" },
    { id: "gpt-5.6-luna-free-thinking", name: "GPT-5.6 Luna — Free Thinking", supportsReasoning: true },
    { id: "gpt-5-5-instant", name: "GPT-5.5 — Instant" },
    { id: "gpt-5-5-thinking", name: "GPT-5.5 — Thinking", supportsReasoning: true },
    { id: "gpt-5-5-pro", name: "GPT-5.5 — Pro", supportsReasoning: true },
    { id: "gpt-image-2", name: "GPT Image 2 (web)", params: ["n", "size"], kind: "image" },
  ],
};
```

### `open-sse/executors/chatgpt-web/sentinel.js`
```javascript
const PREPARE_PREFIX = "gAAAAAC";
const PROOF_PREFIX = "gAAAAAB";
const PROOF_SUFFIX = "~S";
const MAX_POW_ITERATIONS = 500000;

export const SENTINEL_HEADERS = {
  prepare: "OpenAI-Sentinel-Chat-Requirements-Prepare-Token",
  requirements: "OpenAI-Sentinel-Chat-Requirements-Token",
  proof: "OpenAI-Sentinel-Proof-Token",
  turnstile: "OpenAI-Sentinel-Turnstile-Token",
};

/** Sentinel's 32-bit hash: FNV-1a with three avalanche rounds, 8 hex chars. */
export function sentinelHash(input) {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x1000193) >>> 0;
  }
  h ^= h >>> 16;
  h = Math.imul(h, 0x85ebca6b) >>> 0;
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35) >>> 0;
  h ^= h >>> 16;
  return (h >>> 0).toString(16).padStart(8, "0");
}

export function solveProofOfWork({ seed, difficulty, userAgent }) {
  if (!seed || !difficulty) throw new Error("ChatGPT sentinel asked for proof of work without seed/difficulty");
  const started = performance.now();
  const slots = buildFingerprint(userAgent);
  for (let counter = 0; counter < MAX_POW_ITERATIONS; counter++) {
    slots[3] = counter;
    slots[9] = Math.round(performance.now() - started);
    const encoded = encodeFingerprint(slots);
    if (sentinelHash(`${seed}${encoded}`).slice(0, difficulty.length) <= difficulty) {
      return `${PROOF_PREFIX}${encoded}${PROOF_SUFFIX}`;
    }
  }
  throw new Error("ChatGPT sentinel proof of work exhausted its iteration budget");
}

export async function fetchSentinelHeaders({ baseUrl, accessToken, userAgent, signal }) {
  const base = { Authorization: `Bearer ${accessToken}`, "User-Agent": userAgent };
  const prepared = await postJson(`${baseUrl}/backend-api/sentinel/chat-requirements/prepare`, {
    headers: { ...base, [SENTINEL_HEADERS.prepare]: buildPrepareToken(userAgent) },
    signal,
  });
  if (!prepared?.prepare_token) {
    throw Object.assign(new Error("ChatGPT sentinel prepare returned no prepare_token"), { status: 502 });
  }

  const requirements = await postJson(`${baseUrl}/backend-api/sentinel/chat-requirements`, {
    headers: { ...base, [SENTINEL_HEADERS.prepare]: prepared.prepare_token },
    signal,
  });
  const tokens = {};
  if (requirements?.token) tokens[SENTINEL_HEADERS.requirements] = requirements.token;
  if (requirements?.proofofwork?.required) {
    tokens[SENTINEL_HEADERS.proof] = solveProofOfWork({
      seed: requirements.proofofwork.seed,
      difficulty: requirements.proofofwork.difficulty,
      userAgent,
    });
  }
  return tokens;
}
```
