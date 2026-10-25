# ElevenLabs (`elevenlabs`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `elevenlabs`
- **Alias**: `el`
- **Display Name**: ElevenLabs
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["tts", "stt"]`
- **Website**: `https://elevenlabs.io`
- **API Key URL**: `https://elevenlabs.io/app/settings/api-keys`
- **Source Files**:
  - `open-sse/providers/registry/elevenlabs.js`
  - `open-sse/handlers/ttsProviders/elevenlabs.js`
  - `open-sse/handlers/sttCore.js` (`transcribeElevenLabs`)

---

## 2. Endpoints & Authentication
- **Authentication Header**: `xi-api-key: <apiKey>`
- **TTS Endpoints (`open-sse/handlers/ttsProviders/elevenlabs.js`)**:
  - **Voice List**: `GET https://api.elevenlabs.io/v1/voices` (cached 24 hours per API key)
  - **Speech Synthesis**: `POST https://api.elevenlabs.io/v1/text-to-speech/<voiceId>`
    - Splits `model` on `"/"` into `[modelId, voiceId]` if `"/"` is present (defaults `modelId` to `"eleven_flash_v2_5"`).
    - JSON Body:
      ```json
      {
        "text": "<text>",
        "model_id": "<modelId>",
        "voice_settings": {
          "stability": 0.5,
          "similarity_boost": 0.75
        }
      }
      ```
    - Returns `{ base64, format: "mp3" }` (validates response length >= 1024 bytes).
- **STT Endpoint (`transcribeElevenLabs` in `open-sse/handlers/sttCore.js`)**:
  - `POST https://api.elevenlabs.io/v1/speech-to-text`
  - Multipart `FormData` fields:
    - `file`: Audio `Blob`
    - `model_id`: `scribe_v1` or `scribe_v2`
    - `language_code`: Optional (`formData.get("language")`)
    - `timestamps_granularity`: Optional (`"word" | "character" | "none"`)
    - `tag_audio_events`: `"true"` if requested
    - `diarize`: `"true"` (mutually exclusive with `num_speakers`; `diarize` takes precedence)
    - `num_speakers`: Integer `1..32` (when `diarize` is not `"true"`)
    - `additional_formats`: JSON string `[{"format": "seg_json" | "srt" | "vtt"}]` mapped from `response_format` (`verbose_json` -> `"seg_json"`, `srt` -> `"srt"`, `vtt` -> `"vtt"`).

---

## 3. Supported Models
| Model ID | Display Name | Kind | Supported Parameters |
|---|---|---|---|
| `eleven_multilingual_v2` | Eleven Multilingual v2 | `tts` | — |
| `eleven_turbo_v2_5` | Eleven Turbo v2.5 | `tts` | — |
| `scribe_v1` | Scribe v1 | `stt` | `["language", "response_format", "timestamps_granularity", "tag_audio_events", "diarize", "num_speakers"]` |
| `scribe_v2` | Scribe v2 | `stt` | `["language", "response_format", "timestamps_granularity", "tag_audio_events", "diarize", "num_speakers"]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/elevenlabs.js`
```javascript
export default {
  id: "elevenlabs",
  alias: "el",
  display: {
    name: "ElevenLabs",
    icon: "record_voice_over",
    color: "#6C47FF",
    textIcon: "EL",
    website: "https://elevenlabs.io",
    notice: {
      apiKeyUrl: "https://elevenlabs.io/app/settings/api-keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "tts",
    "stt"
  ],
  models: [
    {
      id: "scribe_v1",
      name: "Scribe v1",
      params: ["language", "response_format", "timestamps_granularity", "tag_audio_events", "diarize", "num_speakers"],
      kind: "stt"
    },
    {
      id: "scribe_v2",
      name: "Scribe v2",
      params: ["language", "response_format", "timestamps_granularity", "tag_audio_events", "diarize", "num_speakers"],
      kind: "stt"
    },
  ],
  ttsConfig: {
    baseUrl: "https://api.elevenlabs.io/v1/text-to-speech",
    authType: "apikey",
    authHeader: "xi-api-key",
    format: "elevenlabs",
    models: [
      {
        id: "eleven_multilingual_v2",
        name: "Eleven Multilingual v2"
      },
      {
        id: "eleven_turbo_v2_5",
        name: "Eleven Turbo v2.5"
      }
    ]
  },
  sttConfig: {
    baseUrl: "https://api.elevenlabs.io/v1/speech-to-text",
    authType: "apikey",
    authHeader: "xi-api-key",
    format: "elevenlabs-stt",
    responseFormats: { segments: "seg_json", subtitles: "srt", captions: "vtt" }
  }
}
```

### `open-sse/handlers/ttsProviders/elevenlabs.js`
```javascript
export default {
  async synthesize(text, model, credentials) {
    if (!credentials?.apiKey) throw new Error("ElevenLabs API key required");
    let modelId = "eleven_flash_v2_5";
    let voiceId = model;
    if (model && model.includes("/")) [modelId, voiceId] = model.split("/");

    const res = await fetch(`https://api.elevenlabs.io/v1/text-to-speech/${voiceId}`, {
      method: "POST",
      headers: { "xi-api-key": credentials.apiKey, "Content-Type": "application/json" },
      body: JSON.stringify({
        text,
        model_id: modelId,
        voice_settings: { stability: 0.5, similarity_boost: 0.75 },
      }),
    });
    if (!res.ok) {
      const err = await res.json().catch(() => ({}));
      throw new Error(err?.detail?.message || `ElevenLabs TTS failed: ${res.status}`);
    }
    const buf = await res.arrayBuffer();
    if (buf.byteLength < 1024) throw new Error("ElevenLabs TTS returned empty audio");
    return { base64: Buffer.from(buf).toString("base64"), format: "mp3" };
  },
};
```
