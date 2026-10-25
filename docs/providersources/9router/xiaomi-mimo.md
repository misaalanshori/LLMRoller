# Xiaomi MiMo (`xiaomi-mimo`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `xiaomi-mimo`
- **Display Name**: `Xiaomi MiMo`
- **Aliases**: `mimo`, `mimo-desktop`, `xmd`
- **Category**: `oauth` (`authModes: ["oauth", "apikey"]`)
- **Service Kinds**: `["llm", "tts"]`
- **Executor**: `XiaomiMimoExecutor` (`open-sse/executors/xiaomi-mimo.js`)
- **Account Helper**: `open-sse/shared/mimoAccount.js`
- **TTS Handler**: `open-sse/handlers/ttsProviders/xiaomi-mimo.js`
- **Website**: `https://xiaomimimo.com`

---

## 2. Dual-Route Endpoints & Authentication

### 2.1 Cloud API Route (`api.xiaomimimo.com`)
- **OpenAI Transport**: `POST https://api.xiaomimimo.com/v1/chat/completions` (`Authorization: Bearer <apiKey>`)
- **Claude Native Transport**: `POST https://api.xiaomimimo.com/anthropic/v1/messages` (`x-api-key: <apiKey>`, `anthropic-version: 2023-06-01`)
- **Validation URL**: `GET https://api.xiaomimimo.com/v1/models`

### 2.2 Desktop Account Route (`mimo-server-<region>.xiaomimimo.com`)
Used for dual-route v2.6 models (`mimo-v2.6-pro`, `mimo-v2.6-flash`, `mimo-v2.6-pro-ultraspeed`) when `mimoPassToken` or a desktop account cookie is available:
- **Regional Hosts**:
  - `cn`: `https://mimo-server-cn.xiaomimimo.com` (`sid = "mimopc"`)
  - `sgp` (default): `https://mimo-server-sgp.xiaomimimo.com` (`sid = "mimosgp"`)
  - `ams`: `https://mimo-server-ams.xiaomimimo.com` (`sid = "mimoams"`)
  - `ru`: `https://mimo-server-ru.xiaomimimo.com` (`sid = "mimoru"`)
  - `in`: `https://mimo-server-in.xiaomimimo.com` (`sid = "mimoin"`)
- **Chat Endpoint**: `POST https://mimo-server-{region}.xiaomimimo.com/api/route/chat/completions`
- **Authentication**: Two-phase Xiaomi Passport service login (`GET https://account.xiaomi.com/pass/serviceLogin?_locale=zh_CN&_snsNone=true&sid={clusterSid}&_json=true` with `passToken` cookie -> `GET {location}&clientSign=sha1(nonce & ssecurity)` -> `serviceToken` cookie), sent with `User-Agent: miNative PC/Normal Windows_NT/10.0.19045 SDKV/1.0.0 DEVT/PC DEVS/Windows APP/miaccount_desktop APPV/0.1.0`.
- **OAuth Authorize URL**: `https://platform.xiaomimimo.com/authorize` (`callbackParam: "u"`, ECDH-encrypted callback `kn: "mimocode"`).

---

## 3. Executor Implementation (`open-sse/executors/xiaomi-mimo.js`)

```javascript
const ACCOUNT_MODELS = new Set([
  "mimo-v2.6-pro",
  "mimo-v2.6-flash",
  "mimo-v2.6-pro-ultraspeed",
]);

buildUrl(model, stream, urlIndex = 0, credentials = null) {
  if (this.isAccountRoute(model, credentials)) {
    return `${resolveMimoServerBase(credentials?.providerSpecificData)}/api/route/chat/completions`;
  }
  return super.buildUrl(model, stream, urlIndex, credentials);
}

transformRequest(model, body, stream, credentials) {
  const out = super.transformRequest(model, body, stream, credentials);
  if (this.isAccountRoute(model, credentials)) {
    const rawEffort = out.reasoning_effort || body?.reasoning_effort || body?.output_config?.effort;
    if (rawEffort) {
      delete out.reasoning_effort;
      const norm = String(rawEffort).toLowerCase() === "xhigh" ? "high" : String(rawEffort).toLowerCase();
      out.output_config = { ...(out.output_config || {}), effort: norm };
    }
    if (out.temperature == null) out.temperature = 1.0;
    if (out.top_p == null) out.top_p = 0.95;
  }
  return out;
}
```

---

## 4. TTS Implementation (`open-sse/handlers/ttsProviders/xiaomi-mimo.js`)

Synthesizes speech via `POST https://api.xiaomimimo.com/v1/chat/completions` (`model: "mimo-v2.5-tts"`, `stream: false`) with target text placed in `messages: [{ role: "assistant", content: text }]` (optionally preceded by `{ role: "user", content: instructions }`) and `audio: { format: "wav", voice: voiceId || "mimo_default" }`. Extracts `data.choices[0].message.audio.data`.
