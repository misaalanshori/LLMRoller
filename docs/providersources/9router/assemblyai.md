# AssemblyAI (`assemblyai`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `assemblyai`
- **Alias**: `assemblyai`
- **Aliases**: `["aai"]`
- **UI Alias**: `aai`
- **Display Name**: AssemblyAI
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Service Kinds**: `["stt"]` (Speech-to-Text)
- **Website**: `https://assemblyai.com`
- **API Key URL**: `https://www.assemblyai.com/app/api-keys`
- **STT Core Handler**: `transcribeAssemblyAI()` in `open-sse/handlers/sttCore.js`

## 2. Endpoints & Protocol Flow
- **Validation Endpoint**: `GET https://api.assemblyai.com/v1/account`
- **Audio Upload Endpoint**: `POST https://api.assemblyai.com/v2/upload`
- **Transcript Submit Endpoint**: `POST https://api.assemblyai.com/v2/transcript` (configured via `sttConfig.baseUrl`)
- **Transcript Poll Endpoint**: `GET https://api.assemblyai.com/v2/transcript/{id}`

## 3. Authentication & Headers
- **Auth Config**: `sttConfig.authHeader = "authorization"` -> `buildAuthHeaders()` in `sttCore.js` defaults to `Authorization: Bearer <token>`.
- **Upload Headers**:
  - `Authorization: Bearer <token>`
  - `Content-Type: application/octet-stream`
- **Submit Headers**:
  - `Authorization: Bearer <token>`
  - `Content-Type: application/json`

## 4. Multi-Step Asynchronous STT Protocol
1. **Step 1 — Binary Audio Upload**:
   - Reads the uploaded file `ArrayBuffer` and sends a raw binary `POST https://api.assemblyai.com/v2/upload`.
   - Parses JSON response `{ "upload_url": "<url>" }`.
2. **Step 2 — Job Submission**:
   - Sends `POST https://api.assemblyai.com/v2/transcript` with JSON payload:
     ```json
     {
       "audio_url": "<upload_url>",
       "speech_models": ["<model>"],
       "language_detection": true
     }
     ```
   - Extracts `{ "id": "<transcript_id>" }`.
3. **Step 3 — Polling (Max 120s, 2000ms interval)**:
   - Polls `GET https://api.assemblyai.com/v2/transcript/{id}` every 2 seconds for up to 120,000 ms.
   - When `r.status === "completed"`, returns `{ "text": r.text || "" }`.
   - When `r.status === "error"`, returns HTTP 500 with `r.error`.
   - If 120s elapses, returns HTTP 504 `"AssemblyAI timeout after 120s"`.

## 5. Supported Models
- `universal-3-pro` ("Universal 3 Pro", `params: ["language"]`, `kind: "stt"`)
- `universal-2` ("Universal 2", `params: ["language"]`, `kind: "stt"`)
- `best` ("Best (Nano + Universal)", `kind: "stt"`)
- `nano` ("Nano (Fast)", `kind: "stt"`)

## 6. Source Implementation

### Registry (`open-sse/providers/registry/assemblyai.js`)
```javascript
export default {
  id: "assemblyai",
  priority: 30,
  alias: "assemblyai",
  aliases: [
    "aai",
  ],
  uiAlias: "aai",
  display: {
    name: "AssemblyAI",
    icon: "record_voice_over",
    color: "#0062FF",
    textIcon: "AA",
    website: "https://assemblyai.com",
    notice: {
      apiKeyUrl: "https://www.assemblyai.com/app/api-keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.assemblyai.com/v1/audio/transcriptions",
    validateUrl: "https://api.assemblyai.com/v1/account",
  },
  models: [
    { id: "universal-3-pro", name: "Universal 3 Pro", params: ["language"], kind: "stt" },
    { id: "universal-2", name: "Universal 2", params: ["language"], kind: "stt" },
    { id: "best", name: "Best (Nano + Universal)", kind: "stt" },
    { id: "nano", name: "Nano (Fast)", kind: "stt" },
  ],
  serviceKinds: ["stt"],
  sttConfig: {
    baseUrl: "https://api.assemblyai.com/v2/transcript",
    authType: "apikey",
    authHeader: "authorization",
    format: "assemblyai",
  },
};
```

### STT Handler (`open-sse/handlers/sttCore.js`)
```javascript
// AssemblyAI: upload → submit → poll (max 120s)
async function transcribeAssemblyAI(cfg, file, model, token) {
  const auth = buildAuthHeaders(cfg, token);
  const buf = await file.arrayBuffer();
  const up = await fetch("https://api.assemblyai.com/v2/upload", {
    method: "POST", headers: { ...auth, "Content-Type": "application/octet-stream" }, body: buf,
  });
  if (!up.ok) return upstreamError(up);
  const { upload_url } = await up.json();

  const sub = await fetch(cfg.baseUrl, {
    method: "POST",
    headers: { ...auth, "Content-Type": "application/json" },
    body: JSON.stringify({ audio_url: upload_url, speech_models: [model], language_detection: true }),
  });
  if (!sub.ok) return upstreamError(sub);
  const { id } = await sub.json();

  const start = Date.now();
  while (Date.now() - start < 120_000) {
    await new Promise((r) => setTimeout(r, 2000));
    const poll = await fetch(`${cfg.baseUrl}/${id}`, { headers: auth });
    if (!poll.ok) continue;
    const r = await poll.json();
    if (r.status === "completed") return jsonResponse({ text: r.text || "" });
    if (r.status === "error") return createErrorResult(500, r.error || "AssemblyAI failed");
  }
  return createErrorResult(504, "AssemblyAI timeout after 120s");
}
```
