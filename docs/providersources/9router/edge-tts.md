# Edge TTS (`edge-tts`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `edge-tts`
- **Alias**: `edge-tts`
- **Display Name**: Edge TTS
- **Category**: `freeTier`
- **Auth Type**: `none` (`noAuth: true`)
- **Service Kinds**: `["tts"]`
- **Media Priority**: `5`
- **Source Files**:
  - `open-sse/providers/registry/edge-tts.js`
  - `open-sse/handlers/ttsProviders/edgeTts.js`

---

## 2. Endpoints & Token Harvesting Flow
`edgeTts.js` synthesizes speech without an API key by scraping an abuse-prevention token from Bing Translator and posting SSML to Bing's `tfettts` endpoint:
1. **Token Scrape (`getToken`)**:
   - `GET https://www.bing.com/translator`
   - Headers: `User-Agent: <UA>`, `Accept-Language: vi,en-US;q=0.9,en;q=0.8`
   - Extracts `Set-Cookie` headers and matches `params_AbusePreventionHelper = [key, "token", ...]` in the HTML response. Cached for 5 minutes (`REFRESH_MS = 300000`).
2. **TTS Synthesis Request (`ttsRequest`)**:
   - `POST https://www.bing.com/tfettts?isVertical=1&&IG=1&IID=translator.5023&SFX=1`
   - Headers:
     - `Content-Type`: `application/x-www-form-urlencoded`
     - `Accept`: `*/*`
     - `Origin`: `https://www.bing.com`
     - `Referer`: `https://www.bing.com/translator`
     - `User-Agent`: `<UA>`
     - `Cookie`: `<scraped_cookies>`
   - Form Body (`URLSearchParams`):
     - `ssml`: `<speak version='1.0' xml:lang='${xmlLang}'><voice xml:lang='${xmlLang}' xml:gender='${gender}' name='${voiceId}'><prosody rate='0.00%'>${text}</prosody></voice></speak>`
     - `token`: `<token.token>`
     - `key`: `<token.key>`
   - Automatically invalidates token cache and retries once on HTTP `429` or `403`.
3. **Voices Catalog Endpoint (`fetchEdgeTtsVoices`)**:
   - `GET https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list?trustedclienttoken=6A5AA1D4EAFF4E9FB37E23D68491D6F4`
   - Cached for 24 hours (`VOICES_TTL`).

---

## 3. Verbatim Source Code Snippets

### `open-sse/providers/registry/edge-tts.js`
```javascript
export default {
  id: "edge-tts",
  alias: "edge-tts",
  display: {
    name: "Edge TTS",
    icon: "record_voice_over",
    color: "#0078D4",
    textIcon: "ET"
  },
  category: "freeTier",
  authType: "none",
  serviceKinds: [
    "tts"
  ],
  mediaPriority: 5,
  noAuth: true,
  ttsConfig: {
    baseUrl: "edge-tts",
    authType: "none",
    authHeader: "none",
    format: "edge-tts",
    models: []
  }
};
```

### `open-sse/handlers/ttsProviders/edgeTts.js`
```javascript
async function getToken() {
  const now = Date.now();
  if (cache.token && now - cache.tokenTime < REFRESH_MS) return cache.token;
  const res = await fetch("https://www.bing.com/translator", {
    headers: { "User-Agent": UA, "Accept-Language": "vi,en-US;q=0.9,en;q=0.8" },
  });
  if (!res.ok) throw new Error(`Bing translator fetch failed: ${res.status}`);
  const rawCookies = res.headers.getSetCookie?.() || [];
  const cookie = rawCookies.map((c) => c.split(";")[0]).join("; ");
  const html = await res.text();
  const match = html.match(/params_AbusePreventionHelper\s*=\s*\[([^,]+),([^,]+),/);
  if (!match) throw new Error("Failed to parse Bing token");
  cache.token = { key: match[1], token: match[2].replace(/"/g, ""), cookie };
  cache.tokenTime = now;
  return cache.token;
}

async function ttsRequest(text, voiceId, token) {
  const parts = voiceId.split("-");
  const xmlLang = parts.slice(0, 2).join("-");
  const gender = voiceId.toLowerCase().includes("male") ? "Male" : "Female";
  const ssml = `<speak version='1.0' xml:lang='${xmlLang}'><voice xml:lang='${xmlLang}' xml:gender='${gender}' name='${voiceId}'><prosody rate='0.00%'>${text}</prosody></voice></speak>`;
  const body = new URLSearchParams();
  body.append("ssml", ssml);
  body.append("token", token.token);
  body.append("key", token.key);
  return fetch("https://www.bing.com/tfettts?isVertical=1&&IG=1&IID=translator.5023&SFX=1", {
    method: "POST",
    body: body.toString(),
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      "Accept": "*/*",
      "Origin": "https://www.bing.com",
      "Referer": "https://www.bing.com/translator",
      "User-Agent": UA,
      ...(token.cookie ? { "Cookie": token.cookie } : {}),
    },
  });
}
```
