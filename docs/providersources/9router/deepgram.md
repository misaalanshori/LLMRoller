# Deepgram (`deepgram`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `deepgram`
- **Priority**: `20`
- **Primary Alias**: `deepgram`
- **Additional Aliases**: `["dg"]`
- **UI Alias**: `dg`
- **Display Name**: Deepgram
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["stt"]` (also supports `"deepgram"` TTS format in `genericFormats.js`)
- **Website**: `https://deepgram.com`
- **API Key URL**: `https://console.deepgram.com/api-keys`
- **Source Files**:
  - `open-sse/providers/registry/deepgram.js`
  - `open-sse/handlers/sttCore.js` (`transcribeDeepgram`)
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`deepgram` TTS handler)

---

## 2. Endpoints & Configuration
- **Speech-to-Text (STT) Endpoint**:
  - `POST https://api.deepgram.com/v1/listen?model=<model>&smart_format=true&punctuate=true&<language=XX|detect_language=true>`
  - Overridable via `credentials.providerSpecificData.baseUrl` in `handleSttCore`.
- **Text-to-Speech (TTS) Handler (`genericFormats.js`)**:
  - `POST <baseUrl>?model=<modelId || "aura-asteria-en">` with JSON body `{ "text": "<text>" }`.

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Token <apiKey>` (`authHeader: "token"`)
- **STT Content-Type**: Raw binary audio MIME type resolved by `resolveAudioContentType(file)` (`audio/mpeg`, `audio/mp4`, `audio/wav`, `audio/ogg`, `audio/flac`, `audio/webm`, `audio/aac`, `audio/opus`, or fallback `application/octet-stream`).

---

## 4. Request & Response Format (`transcribeDeepgram`)
- **Request Body**: Raw binary `ArrayBuffer` of the uploaded audio file.
- **Query Parameters**:
  - `model`: Model ID (`nova-3`, `nova-2`, `whisper-large`, `nova`)
  - `smart_format`: `"true"`
  - `punctuate`: `"true"`
  - `language`: Set if `formData.get("language")` is non-empty; otherwise sets `detect_language=true`.
- **Response Extraction**:
  - Extracts `data.results?.channels?.[0]?.alternatives?.[0]?.transcript ?? ""` and returns `{ "text": "<transcript>" }`.

---

## 5. Supported Models
| Model ID | Display Name | Kind | Supported Params |
|---|---|---|---|
| `nova-3` | Nova 3 | `stt` | `["language"]` |
| `nova-2` | Nova 2 | `stt` | `["language"]` |
| `whisper-large` | Whisper Large | `stt` | `["language"]` |
| `nova` | Nova | `stt` | — |

---

## 6. Verbatim Source Code Snippets

### `open-sse/providers/registry/deepgram.js`
```javascript
export default {
  id: "deepgram",
  priority: 20,
  alias: "deepgram",
  aliases: [
    "dg",
  ],
  uiAlias: "dg",
  display: {
    name: "Deepgram",
    icon: "mic",
    color: "#13EF93",
    textIcon: "DG",
    website: "https://deepgram.com",
    notice: {
      text: "$200 free credit on signup (no card required). Aura-1: $0.015/1k chars, Aura-2: $0.030/1k chars (Pay-As-You-Go).",
      apiKeyUrl: "https://console.deepgram.com/api-keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.deepgram.com/v1/listen",
  },
  models: [
    { id: "nova-3", name: "Nova 3", params: ["language"], kind: "stt" },
    { id: "nova-2", name: "Nova 2", params: ["language"], kind: "stt" },
    { id: "whisper-large", name: "Whisper Large", params: ["language"], kind: "stt" },
    { id: "nova", name: "Nova", kind: "stt" },
  ],
  serviceKinds: ["stt"],
  sttConfig: { baseUrl: "https://api.deepgram.com/v1/listen", authType: "apikey", authHeader: "token", format: "deepgram" },
};
```

### `open-sse/handlers/sttCore.js` (`transcribeDeepgram`)
```javascript
// Deepgram: raw binary POST + model query param
async function transcribeDeepgram(cfg, file, model, token, formData) {
  const url = new URL(cfg.baseUrl);
  url.searchParams.set("model", model);
  url.searchParams.set("smart_format", "true");
  url.searchParams.set("punctuate", "true");
  const lang = formData.get("language");
  if (typeof lang === "string" && lang.trim()) url.searchParams.set("language", lang.trim());
  else url.searchParams.set("detect_language", "true");

  const buf = await file.arrayBuffer();
  const res = await fetch(url, {
    method: "POST",
    headers: { ...buildAuthHeaders(cfg, token), "Content-Type": resolveAudioContentType(file) },
    body: buf,
  });
  if (!res.ok) return upstreamError(res);
  const data = await res.json();
  const text = data.results?.channels?.[0]?.alternatives?.[0]?.transcript ?? "";
  return jsonResponse({ text });
}
```
