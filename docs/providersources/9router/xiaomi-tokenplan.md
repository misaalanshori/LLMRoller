# Xiaomi MiMo (Token Plan) (`xiaomi-tokenplan`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `xiaomi-tokenplan`
- **Display Name**: `Xiaomi MiMo (Token Plan)`
- **Alias**: `xmtp`
- **Category**: `apikey` (`hasProviderSpecificData: true`)
- **Service Kinds**: `["llm"]`
- **Executor**: `XiaomiTokenplanExecutor` (`open-sse/executors/xiaomi-tokenplan.js`)
- **Website**: `https://mimo.xiaomi.com`

---

## 2. Regional Endpoints & Dual-Transport Routing

| Region | Base URL |
|---|---|
| `sgp` (Default) | `https://token-plan-sgp.xiaomimimo.com/v1` |
| `cn` | `https://token-plan-cn.xiaomimimo.com/v1` |
| `ams` | `https://token-plan-ams.xiaomimimo.com/v1` |

- **OpenAI Transport**: `POST https://token-plan-{region}.xiaomimimo.com/v1/chat/completions` (`Authorization: Bearer <apiKey>`)
- **Claude Native Transport**: `POST https://token-plan-{region}.xiaomimimo.com/anthropic/v1/messages` (`x-api-key: <apiKey>`, `anthropic-version: 2023-06-01`)

---

## 3. Executor Implementation (`open-sse/executors/xiaomi-tokenplan.js`)

```javascript
export class XiaomiTokenplanExecutor extends DefaultExecutor {
  constructor() {
    super("xiaomi-tokenplan");
  }

  // Token Plan keys are region-specific. Route per sourceFormat-matched transport:
  // claude → Anthropic /anthropic/v1/messages, openai → /chat/completions.
  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    const baseUrl = resolveXiaomiTokenplanBaseUrl(credentials);
    if (credentials?.runtimeTransport?.format === "claude") {
      return `${baseUrl.replace(/\/v1\/?$/, "")}/anthropic/v1/messages`;
    }
    return `${baseUrl}/chat/completions`;
  }
}
```

---

## 4. Supported Models (`open-sse/providers/registry/xiaomi-tokenplan.js`)

- `mimo-v2.5-pro` (`MiMo V2.5 Pro`)
- `mimo-v2.5-pro-claude` (`MiMo V2.5 Pro (Claude Native)`, `targetFormat: "claude"`, `upstreamModelId: "mimo-v2.5-pro"`)
- `mimo-v2.5` (`MiMo V2.5`)
- `mimo-v2-pro` (`MiMo V2 Pro`)
- `mimo-v2-omni` (`MiMo V2 Omni`)
- `mimo-v2-tts` (`MiMo V2 TTS`)
- `mimo-v2.5-tts` (`MiMo V2.5 TTS`)
- `mimo-v2.5-tts-voiceclone` (`MiMo V2.5 TTS Voice Clone`)
- `mimo-v2.5-tts-voicedesign` (`MiMo V2.5 TTS Voice Design`)
