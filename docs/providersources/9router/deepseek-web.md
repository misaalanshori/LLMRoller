# DeepSeek Web (`deepseek-web`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `deepseek-web`
- **Priority**: `220`
- **Primary Alias**: `deepseek-web`
- **Additional Aliases**: `["ds-web"]`
- **UI Alias**: `deepseek-web`
- **Display Name**: DeepSeek Web
- **Category**: `webCookie`
- **Auth Type**: `cookie` (`userToken` from `chat.deepseek.com` `localStorage`)
- **Website**: `https://chat.deepseek.com`
- **Executor**: `DeepSeekWebExecutor` (`open-sse/executors/deepseek-web.js`)
- **Source Files**:
  - `open-sse/providers/registry/deepseek-web.js`
  - `open-sse/executors/deepseek-web.js`
  - `open-sse/executors/deepseek-web/pow.js`
  - `open-sse/executors/deepseek-web/stream-format.js`
  - `open-sse/lib/deepseek-pow.js`
  - `open-sse/translator/deepseekWebTools.js`

---

## 2. Multi-Step Web API Protocol & Endpoints
1. **Access Token Exchange (`acquireAccessToken`)**:
   - `GET https://chat.deepseek.com/api/v0/users/current`
   - Headers: `Authorization: Bearer <userToken>` + `FAKE_HEADERS`
   - Extracts `json.data.biz_data.token` and caches it for 3600 seconds.
   - Note: `extractUserToken` accepts either a raw string token or DeepSeek's localStorage JSON object `{"value": "<token>"}`.
2. **Chat Session Creation (`createSession`)**:
   - `POST https://chat.deepseek.com/api/v0/chat_session/create`
   - Body: `{}`
   - Extracts `json.data.biz_data.chat_session.id`.
3. **Proof-of-Work Challenge Request (`requestDeepSeekPowChallenge`)**:
   - `POST https://chat.deepseek.com/api/v0/chat/create_pow_challenge`
   - Body: `{ "target_path": "/api/v0/chat/completion" }`
   - Returns `challenge`: `{ algorithm, challenge, salt, difficulty, expire_at, signature }`.
4. **PoW Solver (`solvePow` / `solveDeepSeekPowAsync`)**:
   - Solves `DeepSeekHashV1` challenge and Base64-encodes `{ algorithm, challenge, salt, answer, signature, target_path }` into the `X-Ds-Pow-Response` header.
5. **Chat Completion Stream**:
   - `POST https://chat.deepseek.com/api/v0/chat/completion`
   - Headers: `FAKE_HEADERS` + `Authorization: Bearer <accessToken>`, `X-Ds-Pow-Response: <base64Pow>`, `X-Client-Timezone-Offset: <seconds>`, `Cookie: intercom-HWWAFSESTIME=<ts>; HWWAFSESID=<18hex>; Hm_lvt_<uuid>=<sec>; _frid=<uuid>`
   - Request Body:
     ```json
     {
       "chat_session_id": "<sessionId>",
       "parent_message_id": null,
       "model_type": "default | expert",
       "prompt": "<flattened_prompt>",
       "ref_file_ids": [],
       "thinking_enabled": true,
       "search_enabled": false,
       "preempt": false
     }
     ```
6. **Session Cleanup (`deleteSessionOnDeepSeek`)**:
   - `POST https://chat.deepseek.com/api/v0/chat_session/delete` with `{ "chat_session_id": "<sessionId>" }` (unless `providerSpecificData.persistSession === true`).

---

## 3. Fingerprint Headers (`FAKE_HEADERS`)
```javascript
const FAKE_HEADERS = {
  Accept: "*/*",
  "Accept-Encoding": "gzip, deflate, br, zstd",
  "Accept-Language": "en-US,en;q=0.9",
  Origin: "https://chat.deepseek.com",
  Referer: "https://chat.deepseek.com/",
  "User-Agent":
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36",
  "X-Client-Bundle-Id": "com.deepseek.chat",
  "X-Client-Locale": "en-US",
  "X-Client-Platform": "web",
  "X-Client-Version": "2.0.0",
};
```

---

## 4. Supported Models & Mode Resolution (`resolveModelOptions`)
- `modelType` is `"expert"` when model ID contains `"pro"` or `"expert"`, otherwise `"default"`.
- `thinkingEnabled` is `true` when model ID contains `"r1"`, `"think"`, or `"reason"` (or `body.thinking_enabled` / `body.reasoning_effort` is set).
- `searchEnabled` is `true` when model ID contains `"search"` (or `body.search_enabled` / `body.web_search` is set).

| Model ID | Display Name | Supports Reasoning |
|---|---|---|
| `deepseek-v4-pro` | DeepSeek V4 Pro | `false` (`model_type: "expert"`) |
| `deepseek-v4-pro-think` | DeepSeek V4 Pro Think | `true` (`model_type: "expert"`) |
| `deepseek-v4-pro-search` | DeepSeek V4 Pro Search | `false` (`model_type: "expert"`, `search: true`) |
| `deepseek-v4-pro-think-search` | DeepSeek V4 Pro Think+Search | `true` (`model_type: "expert"`, `search: true`) |
| `deepseek-v4-flash` | DeepSeek V4 Flash | `false` |
| `deepseek-v4-flash-think` | DeepSeek V4 Flash Think | `true` |
| `deepseek-v4-flash-search` | DeepSeek V4 Flash Search | `false` (`search: true`) |
| `deepseek-v4-flash-think-search` | DeepSeek V4 Flash Think+Search | `true` (`search: true`) |
| `deepseek-chat` | DeepSeek Chat | `false` |
| `deepseek-reasoner` | DeepSeek Reasoner | `true` |
| `DeepSeek-R1` | DeepSeek R1 | `true` |
| `DeepSeek-R1-Search` | DeepSeek R1 Search | `true` (`search: true`) |
| `DeepSeek-V3.2` | DeepSeek V3.2 | `false` |
| `DeepSeek-Search` | DeepSeek Search | `false` (`search: true`) |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/deepseek-web/pow.js`
```javascript
export async function requestDeepSeekPowChallenge(options) {
  const response = await fetch("https://chat.deepseek.com/api/v0/chat/create_pow_challenge", {
    method: "POST",
    headers: {
      ...options.headers,
      "Content-Type": "application/json",
      Authorization: `Bearer ${options.accessToken}`,
    },
    body: JSON.stringify({ target_path: options.targetPath }),
    signal: options.signal ?? undefined,
  });
  if (!response.ok) throw new Error(`create_pow_challenge HTTP ${response.status}`);
  const json = await response.json();
  const challenge = json?.data?.biz_data?.challenge ?? json?.biz_data?.challenge;
  const candidate = { ...challenge, target_path: options.targetPath };
  return candidate;
}
```

### `open-sse/executors/deepseek-web.js` (`performCompletion` Excerpt)
```javascript
      const performCompletion = async (sid, completionPrompt = prompt) => {
        const powChallenge = await getPowChallenge(accessToken, signal);
        const powAnswer = await solvePow(powChallenge, signal);
        const reqHeaders = {
          ...FAKE_HEADERS,
          "Content-Type": "application/json",
          Authorization: `Bearer ${accessToken}`,
          "X-Ds-Pow-Response": powAnswer,
          "X-Client-Timezone-Offset": String(new Date().getTimezoneOffset() * -60),
          Cookie: generateFakeCookie(),
        };
        const requestPayload = {
          chat_session_id: sid,
          parent_message_id: null,
          model_type: modelType,
          prompt: completionPrompt,
          ref_file_ids: Array.isArray(bodyObj.ref_file_ids) ? bodyObj.ref_file_ids : [],
          thinking_enabled: thinkingEnabled,
          search_enabled: searchEnabled,
          preempt: false,
        };
        const resp = await fetch(COMPLETION_URL, {
          method: "POST",
          headers: reqHeaders,
          body: JSON.stringify(requestPayload),
          signal: signal ?? undefined,
        });
        return { resp, reqHeaders, requestPayload };
      };
```
