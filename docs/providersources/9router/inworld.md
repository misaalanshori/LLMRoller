# Inworld TTS (`inworld`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `inworld`
- **Alias**: `inworld`
- **Display Name**: Inworld TTS
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["tts"]`
- **Website**: `https://inworld.ai`
- **API Key URL**: `https://platform.inworld.ai/api-keys`
- **Source Files**:
  - `open-sse/providers/registry/inworld.js`
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`inworld` format handler)

---

## 2. Endpoints & Configuration
- **TTS Endpoint**: `POST https://api.inworld.ai/tts/v1/voice`
- **Format Identifier**: `"inworld"`
- **Auth Header Scheme**: HTTP Basic (`Authorization: Basic <apiKey>`)

---

## 3. Request & Response Format (`genericFormats.js`)
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Basic <apiKey>`
- **Request JSON Body**:
  ```json
  {
    "text": "<text>",
    "voiceId": "<voiceId || 'Alex'>",
    "modelId": "<modelId || 'inworld-tts-1.5-mini'>",
    "audioConfig": {
      "audioEncoding": "MP3"
    }
  }
  ```
- **Response Format**:
  - Parses JSON `{ "audioContent": "<base64_mp3>" }` and returns `{ base64: data.audioContent, format: "mp3" }`.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `inworld-tts-1.5-mini` | Inworld TTS 1.5 Mini ($0.01/min) |
| `inworld-tts-1.5-max` | Inworld TTS 1.5 Max ($0.025/min) |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/inworld.js`
```javascript
export default {
  id: "inworld",
  alias: "inworld",
  display: {
    name: "Inworld TTS",
    icon: "record_voice_over",
    color: "#FF6B6B",
    textIcon: "IW",
    website: "https://inworld.ai",
    notice: {
      text: "Free tier: 40 minutes/month TTS. Paid: TTS-1.5 Mini $0.01/min ($15/1M chars), TTS-1.5 Max $0.025/min ($30/1M chars). 270+ voices, 15 languages.",
      apiKeyUrl: "https://platform.inworld.ai/api-keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "tts"
  ],
  ttsConfig: {
    baseUrl: "https://api.inworld.ai/tts/v1/voice",
    authType: "apikey",
    authHeader: "basic",
    format: "inworld",
    models: [
      {
        id: "inworld-tts-1.5-mini",
        name: "Inworld TTS 1.5 Mini ($0.01/min)"
      },
      {
        id: "inworld-tts-1.5-max",
        name: "Inworld TTS 1.5 Max ($0.025/min)"
      }
    ]
  }
};
```

### `open-sse/handlers/ttsProviders/genericFormats.js` (`inworld`)
```javascript
// Inworld: Basic auth, JSON { audioContent }
async function inworld({ baseUrl, apiKey, text, modelId, voiceId }) {
  const res = await fetch(baseUrl, {
    method: "POST",
    headers: { "Content-Type": "application/json", "Authorization": `Basic ${apiKey}` },
    body: JSON.stringify({
      text,
      voiceId: voiceId || "Alex",
      modelId: modelId || "inworld-tts-1.5-mini",
      audioConfig: { audioEncoding: "MP3" },
    }),
  });
  if (!res.ok) await throwUpstreamError(res);
  const data = await res.json();
  if (!data.audioContent) throw new Error("Inworld TTS returned no audio");
  return { base64: data.audioContent, format: "mp3" };
}
```
