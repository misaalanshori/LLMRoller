# Grok Web (`grok-web`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `grok-web`
- **Priority**: `150`
- **Primary Alias**: `grok-web`
- **Additional Aliases**: `["gw"]`
- **UI Alias**: `gw`
- **Display Name**: Grok Web (Subscription)
- **Category**: `webCookie`
- **Auth Type**: `cookie` (`sso`, `sso-rw`, `cf_clearance`, `__cf_bm` from `grok.com`)
- **Service Kinds**: `["image", "video"]` (`videoConfig: { executorDriven: true }`; LLM chat is also fully implemented in `GrokWebExecutor`)
- **Website**: `https://grok.com`
- **Executor**: `GrokWebExecutor` (`open-sse/executors/grok-web.js`)
- **Source Files**:
  - `open-sse/providers/registry/grok-web.js`
  - `open-sse/executors/grok-web.js`
  - `open-sse/executors/grok-web-tools.js`
  - `open-sse/services/grokTlsClient.js`

---

## 2. Endpoints & Chrome-TLS Impersonation (`grokTlsFetch`)
- **Primary Endpoint**: `POST https://grok.com/rest/app-chat/conversations/new`
- **Asset Download Host**: `GET https://assets.grok.com/<imageUrl_or_videoUrl>`
- **Transport**: Uses `grokTlsFetch` (`open-sse/services/grokTlsClient.js`) to impersonate a real Chrome TLS JA3/HTTP2 fingerprint so Cloudflare does not block requests carrying `cf_clearance`.

---

## 3. Authentication & Anti-Bot Headers
- **Cookie Header**: Built via `buildGrokCookieHeader(credentials.apiKey)` combining `sso`, `sso-rw`, `cf_clearance`, and `__cf_bm`.
- **Fingerprint Headers**:
  - `Baggage`: `"sentry-environment=production,sentry-release=d6add6fb0460641fd482d767a335ef72b9b6abb8,sentry-public_key=b311e0f2690c81f25e2c4cf6d4f7ce1c"`
  - `Origin`: `"https://grok.com"`
  - `Referer`: `"https://grok.com/"`
  - `Sec-Ch-Ua`: `'"Chromium";v="146", "Not(A:Brand";v="24"'`
  - `Sec-Ch-Ua-Mobile`: `"?0"`
  - `Sec-Ch-Ua-Platform`: `'"macOS"'`
  - `User-Agent`: `GROK_TLS_USER_AGENT`
  - `x-statsig-id`: `btoa("x1:TypeError: Cannot read properties of null (reading 'children[\"<5chars>\"]')")` (synthetic base64-encoded TypeError string matching Grok Web's Statsig telemetry header)
  - `Browser-Id`: `<uuid>`
  - `x-xai-request-id`: `<uuid>`
  - `traceparent`: `00-<32hex>-<16hex>-00`

---

## 4. Request Payloads (Chat, Image, Video)
1. **Chat & Image Generation Payload**:
   - All chat/image models route to `modelName: "grok-4-1-thinking-1129"` with varying `modelMode` (`MODEL_MODE_FAST`, `MODEL_MODE_GROK_4_1_MINI_THINKING`, `MODEL_MODE_GROK_4_1_THINKING`, `MODEL_MODE_EXPERT`, `MODEL_MODE_GROK_420`).
   - For image generation (`kind === "image"`), sets `enableImageGeneration: true`, `enableImageStreaming: true`, `imageGenerationCount: 1`, and extracts `outer.result.response.cardAttachment.jsonData` -> `inner.image_chunk.imageUrl` when `progress >= 100`.
2. **Grok Imagine Video Payload (`buildGrokVideoPayload`)**:
   ```json
   {
     "modelName": "imagine-video-gen",
     "message": "<prompt> --mode=custom",
     "enableImageStreaming": true,
     "enableSideBySide": true,
     "sendFinalMetadata": true,
     "responseMetadata": { "experiments": [], "modelConfigOverride": { "modelMap": {} } },
     "mediaGenInput": {
       "textToVideo": {
         "prompt": "<prompt>",
         "aspectRatio": "16:9",
         "duration": 6,
         "resolutionName": "480p"
       }
     },
     "kind": "CONVERSATION_KIND_IMAGINE"
   }
   ```

---

## 5. Supported Models
| Model ID | Display Name | Kind | Upstream `modelName` / `modelMode` |
|---|---|---|---|
| `grok-image` | Grok Imagine Image | `image` | `grok-4-1-thinking-1129` (`MODEL_MODE_FAST`, `enableImageGeneration: true`) |
| `grok-video` | Grok Imagine Video | `video` | `imagine-video-gen` (`CONVERSATION_KIND_IMAGINE`) |
| `grok-3`, `grok-4`, `grok-4.1-fast` | Grok Fast | `llm` | `grok-4-1-thinking-1129` / `MODEL_MODE_FAST` |
| `grok-3-mini`, `grok-4-mini`, `grok-4.1-mini` | Grok Mini Thinking | `llm` | `grok-4-1-thinking-1129` / `MODEL_MODE_GROK_4_1_MINI_THINKING` |
| `grok-3-thinking`, `grok-4-thinking`, `grok-4.1-thinking` | Grok Thinking | `llm` | `grok-4-1-thinking-1129` / `MODEL_MODE_GROK_4_1_THINKING` |
| `grok-4-heavy`, `grok-4.1-expert` | Grok Expert / Heavy | `llm` | `grok-4-1-thinking-1129` / `MODEL_MODE_EXPERT` |
| `grok-4.2` | Grok 4.2 (4.20 Beta) | `llm` | `grok-4-1-thinking-1129` / `MODEL_MODE_GROK_420` |

---

## 6. Verbatim Source Code Snippets

### `open-sse/executors/grok-web.js` (Excerpt)
```javascript
function buildGrokVideoPayload({ prompt, size, duration }) {
  const aspectRatio = /^\d{1,2}:\d{1,2}$/.test(String(size || "")) ? String(size) : "16:9";
  return {
    modelName: "imagine-video-gen",
    message: `${prompt} --mode=custom`,
    enableImageStreaming: true,
    enableSideBySide: true,
    sendFinalMetadata: true,
    responseMetadata: { experiments: [], modelConfigOverride: { modelMap: {} } },
    mediaGenInput: {
      textToVideo: {
        prompt,
        aspectRatio,
        duration: Number.isFinite(Number(duration)) && Number(duration) > 0 ? Number(duration) : 6,
        resolutionName: GROK_VIDEO_RESOLUTION,
      },
    },
    kind: "CONVERSATION_KIND_IMAGINE",
  };
}

function generateStatsigId() {
  const msg = Math.random() < 0.5
    ? `x1:TypeError: Cannot read properties of null (reading 'children["${randomString(5, true)}"]')`
    : `x1:TypeError: Cannot read properties of undefined (reading '${randomString(10)}')`;
  return btoa(msg);
}
```
