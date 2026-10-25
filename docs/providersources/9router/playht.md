# PlayHT (`playht`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `playht`
- **Alias**: `playht`
- **Display Name**: PlayHT
- **Category**: `apikey`
- **Auth Type**: `apikey` (composite credential format `"userId:apiKey"`)
- **Service Kinds**: `["tts"]`
- **Hidden**: `true`
- **Website**: `https://play.ht`
- **API Key URL**: `https://play.ht/studio/api-access`
- **Source Files**:
  - `open-sse/providers/registry/playht.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`playht` format handler)

---

## 2. Endpoints & Configuration
- **TTS Stream Endpoint**: `POST https://api.play.ht/api/v2/tts/stream`
- **Format Identifier**: `"playht"`

---

## 3. Authentication, Headers & Request Format
- **Composite Credential Parsing**: Splits `apiKey` on `":"` into `[userId, key]`.
- **Headers**:
  - `Content-Type`: `"application/json"`
  - `Accept`: `"audio/mpeg"`
  - `X-USER-ID`: `<userId>`
  - `Authorization`: `Bearer <key || apiKey>`
- **Request JSON Payload**:
  ```json
  {
    "text": "<text>",
    "voice": "<voiceId || 's3://voice-cloning-zero-shot/d9ff78ba-d016-47f6-b0ef-dd630f59414e/female-cs/manifest.json'>",
    "voice_engine": "<modelId || 'PlayDialog'>",
    "output_format": "mp3",
    "speed": 1
  }
  ```
- **Response Format**: Raw MP3 audio stream converted to `{ base64, format: "mp3" }` via `responseToBase64(res, "mp3")`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `PlayDialog` | PlayDialog |
| `Play3.0-mini` | Play 3.0 Mini |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/playht.js`
```javascript
export default {
  id: "playht",
  alias: "playht",
  display: {
    name: "PlayHT",
    icon: "play_circle",
    color: "#00B4D8",
    textIcon: "PH",
    website: "https://play.ht",
    notice: {
      apiKeyUrl: "https://play.ht/studio/api-access"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "tts"
  ],
  ttsConfig: {
    baseUrl: "https://api.play.ht/api/v2/tts/stream",
    authType: "apikey",
    authHeader: "playht",
    format: "playht",
    models: [
      {
        id: "PlayDialog",
        name: "PlayDialog"
      },
      {
        id: "Play3.0-mini",
        name: "Play 3.0 Mini"
      }
    ]
  },
  hidden: true
};
```

### `open-sse/handlers/ttsProviders/genericFormats.js` (`playht`)
```javascript
// PlayHT: token format "userId:apiKey", voice = s3 URL
async function playht({ baseUrl, apiKey, text, modelId, voiceId }) {
  const [userId, key] = (apiKey || ":").split(":");
  const res = await fetch(baseUrl, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "Accept": "audio/mpeg",
      "X-USER-ID": userId || "",
      "Authorization": `Bearer ${key || apiKey}`,
    },
    body: JSON.stringify({
      text,
      voice: voiceId || "s3://voice-cloning-zero-shot/d9ff78ba-d016-47f6-b0ef-dd630f59414e/female-cs/manifest.json",
      voice_engine: modelId || "PlayDialog",
      output_format: "mp3",
      speed: 1,
    }),
  });
  if (!res.ok) await throwUpstreamError(res);
  return responseToBase64(res, "mp3");
}
```
