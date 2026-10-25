# Cartesia (`cartesia`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `cartesia`
- **Alias**: `cartesia`
- **Display Name**: Cartesia
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["tts"]`
- **Hidden**: `true`
- **Website**: `https://cartesia.ai`
- **API Key URL**: `https://play.cartesia.ai/keys`
- **Source Files**:
  - `open-sse/providers/registry/cartesia.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`cartesia` format handler)

---

## 2. Endpoints & Configuration
- **TTS Endpoint**: `POST https://api.cartesia.ai/tts/bytes`
- **TTS Format Identifier**: `"cartesia"`

---

## 3. Authentication & Headers
- **API Key Header**: `X-API-Key: <apiKey>`
- **API Version Header**: `Cartesia-Version: 2024-06-10`
- **Content-Type**: `application/json`

---

## 4. Request & Response Format
- **Request JSON Payload**:
  ```json
  {
    "model_id": "sonic-2",
    "transcript": "<text>",
    "voice": {
      "mode": "id",
      "id": "<voiceId>"
    },
    "output_format": {
      "container": "mp3",
      "bit_rate": 128000,
      "sample_rate": 44100
    }
  }
  ```
  - Note: `model_id` defaults to `"sonic-2"` if `modelId` is omitted. `voice` object is only included when `voiceId` is truthy.
- **Response Format**: Raw audio bytes (`mp3`), converted to `{ base64, format: "mp3" }` via `responseToBase64(res, "mp3")`.

---

## 5. Supported Models
| Model ID | Display Name |
|---|---|
| `sonic-2` | Sonic 2 |
| `sonic-3` | Sonic 3 |

---

## 6. Verbatim Source Code Snippets

### `open-sse/providers/registry/cartesia.js`
```javascript
export default {
  id: "cartesia",
  alias: "cartesia",
  display: {
    name: "Cartesia",
    icon: "spatial_audio",
    color: "#FF4F8B",
    textIcon: "CA",
    website: "https://cartesia.ai",
    notice: {
      apiKeyUrl: "https://play.cartesia.ai/keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "tts"
  ],
  ttsConfig: {
    baseUrl: "https://api.cartesia.ai/tts/bytes",
    authType: "apikey",
    authHeader: "x-api-key",
    format: "cartesia",
    models: [
      {
        id: "sonic-2",
        name: "Sonic 2"
      },
      {
        id: "sonic-3",
        name: "Sonic 3"
      }
    ]
  },
  hidden: true
};
```

### `open-sse/handlers/ttsProviders/genericFormats.js` (`cartesia`)
```javascript
// Cartesia: X-API-Key header
async function cartesia({ baseUrl, apiKey, text, modelId, voiceId }) {
  const res = await fetch(baseUrl, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-API-Key": apiKey,
      "Cartesia-Version": "2024-06-10",
    },
    body: JSON.stringify({
      model_id: modelId || "sonic-2",
      transcript: text,
      ...(voiceId ? { voice: { mode: "id", id: voiceId } } : {}),
      output_format: { container: "mp3", bit_rate: 128000, sample_rate: 44100 },
    }),
  });
  if (!res.ok) await throwUpstreamError(res);
  return responseToBase64(res, "mp3");
}
```
