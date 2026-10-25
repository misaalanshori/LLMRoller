# Tortoise TTS (`tortoise`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `tortoise`
- **Display Name**: `Tortoise TTS`
- **Alias**: `ttts`
- **Category**: `apikey` (`noAuth: true`)
- **Service Kinds**: `["audio"]` (`subKind: "tts"`)
- **Website**: `https://github.com/neonbjb/tortoise-tts`

---

## 2. Endpoints & Request Builder

| Parameter | Value |
|---|---|
| **Endpoint** | `POST ${baseUrl}/api/tts` (Default `http://localhost:5000/api/tts`) |
| **Auth Method** | None (`noAuth: true`) |
| **Default Model** | `tortoise-v2` |
| **Output Format** | `audio/wav` |
| **Voices** | `random`, `angie`, `daniel`, `deniro`, `emma`, `freeman`, `geralt`, `halle`, `jlaw`, `lj`, `mol`, `myself`, `pat`, `rainbow`, `snakes`, `tim_reynolds`, `tom`, `weaver`, `william` |

---

## 3. Implementation (`open-sse/config/ttsRegistry.js` & `open-sse/services/ttsFormats/genericFormats.js`)

```javascript
tortoise: {
  id: "tortoise",
  name: "Tortoise TTS",
  format: "tortoise",
  baseUrl: "http://localhost:5000",
  authType: "none",
  noAuth: true,
  defaultModel: "tortoise-v2",
  models: [
    { id: "tortoise-v2", name: "Tortoise v2 (Standard)" },
  ],
},
```

```javascript
export function buildTortoiseRequest({ text, voiceId, baseUrl }) {
  const base = (baseUrl || "http://localhost:5000").replace(/\/$/, "");
  return {
    url: `${base}/api/tts`,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text, voice: voiceId || "random" }),
  };
}
```
