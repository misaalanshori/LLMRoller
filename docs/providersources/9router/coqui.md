# Coqui TTS (`coqui`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `coqui`
- **Alias**: `coqui`
- **Display Name**: Coqui TTS
- **Category**: `freeTier`
- **Auth Type**: `none` (`noAuth: true`)
- **Service Kinds**: `["tts"]`
- **Hidden**: `true`
- **Website**: `https://github.com/coqui-ai/TTS`
- **Source Files**:
  - `open-sse/providers/registry/coqui.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`coqui` format handler)

---

## 2. Endpoints & Configuration
- **TTS Endpoint**: `POST http://localhost:5002/api/tts`
- **Format Identifier**: `"coqui"`
- **Authentication**: None (`noAuth: true`, `authHeader: "none"`)
- **Headers**: `Content-Type: application/json`

---

## 3. Request & Response Format
- **Request JSON Payload**:
  ```json
  {
    "text": "<text>",
    "speaker_id": "<voiceId>"
  }
  ```
  *(Note: `speaker_id` is only included if `voiceId` is provided).*
- **Response Format**: Raw WAV audio bytes, converted to `{ base64, format: "wav" }` via `responseToBase64(res, "wav")`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `tts_models/en/ljspeech/tacotron2-DDC` | Tacotron2 DDC (LJSpeech) |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/coqui.js`
```javascript
export default {
  id: "coqui",
  alias: "coqui",
  display: {
    name: "Coqui TTS",
    icon: "record_voice_over",
    color: "#10B981",
    textIcon: "CQ",
    website: "https://github.com/coqui-ai/TTS"
  },
  category: "freeTier",
  authType: "none",
  serviceKinds: [
    "tts"
  ],
  noAuth: true,
  ttsConfig: {
    baseUrl: "http://localhost:5002/api/tts",
    authType: "none",
    authHeader: "none",
    format: "coqui",
    models: [
      {
        id: "tts_models/en/ljspeech/tacotron2-DDC",
        name: "Tacotron2 DDC (LJSpeech)"
      }
    ]
  },
  hidden: true
};
```

### `open-sse/handlers/ttsProviders/genericFormats.js` (`coqui`)
```javascript
// Coqui (local, noAuth): POST { text, speaker_id } → WAV
async function coqui({ baseUrl, text, voiceId }) {
  const res = await fetch(baseUrl, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text, ...(voiceId ? { speaker_id: voiceId } : {}) }),
  });
  if (!res.ok) await throwUpstreamError(res);
  return responseToBase64(res, "wav");
}
```
