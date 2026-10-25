# Fish Audio (`fish-audio`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `fish-audio`
- **Alias**: `fish`
- **Display Name**: Fish Audio
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["tts"]`
- **Website**: `https://fish.audio`
- **API Key URL**: `https://fish.audio/app/api-keys/`
- **Source Files**:
  - `open-sse/providers/registry/fish-audio.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`fishAudio` format handler)

---

## 2. Endpoints & Configuration
- **TTS Endpoint**: `POST https://api.fish.audio/v1/tts`
- **Format Identifier**: `"fish-audio"`

---

## 3. Authentication, Headers & Request Format
Fish Audio passes the model ID via an HTTP header (`model: <modelId>`) rather than inside the JSON payload, while the voice ID is sent as `reference_id` in the body:
- **Headers**:
  - `Content-Type`: `application/json`
  - `Authorization`: `Bearer <apiKey>`
  - `model`: `<modelId || "s2.1-pro-free">`
- **Request Body**:
  ```json
  {
    "text": "<text>",
    "format": "mp3",
    "reference_id": "<voiceId>"
  }
  ```
  *(Note: `reference_id` is omitted if `voiceId` is falsy).*
- **Response Format**: Raw MP3 audio bytes converted to `{ base64, format: "mp3" }` via `responseToBase64(res, "mp3")`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `s2.1-pro-free` | S2.1 Pro Free |
| `s2.1-pro` | S2.1 Pro |
| `s2-pro` | S2 Pro |
| `s1` | S1 |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/fish-audio.js`
```javascript
export default {
  id: "fish-audio",
  alias: "fish",
  display: {
    name: "Fish Audio",
    icon: "record_voice_over",
    color: "#1E9BF0",
    textIcon: "FA",
    website: "https://fish.audio",
    notice: {
      apiKeyUrl: "https://fish.audio/app/api-keys/",
    },
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: ["tts"],
  ttsConfig: {
    baseUrl: "https://api.fish.audio/v1/tts",
    authType: "apikey",
    authHeader: "bearer",
    format: "fish-audio",
    models: [
      { id: "s2.1-pro-free", name: "S2.1 Pro Free" },
      { id: "s2.1-pro", name: "S2.1 Pro" },
      { id: "s2-pro", name: "S2 Pro" },
      { id: "s1", name: "S1" },
    ],
  },
};
```

### `open-sse/handlers/ttsProviders/genericFormats.js` (`fishAudio`)
```javascript
// Fish Audio: model travels in an HTTP header, the voice is a reference_id, returns binary
async function fishAudio({ baseUrl, apiKey, text, modelId, voiceId }) {
  const res = await fetch(baseUrl, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${apiKey}`,
      "model": modelId || "s2.1-pro-free",
    },
    body: JSON.stringify({
      text,
      format: "mp3",
      ...(voiceId ? { reference_id: voiceId } : {}),
    }),
  });
  if (!res.ok) await throwUpstreamError(res);
  return responseToBase64(res, "mp3");
}
```
