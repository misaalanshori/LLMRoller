# Black Forest Labs (`black-forest-labs`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `black-forest-labs`
- **Alias**: `black-forest-labs`
- **Aliases**: `["bfl"]`
- **UI Alias**: `bfl`
- **Display Name**: Black Forest Labs
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Service Kinds**: `["image"]`
- **Website**: `https://blackforestlabs.ai`
- **API Key URL**: `https://api.bfl.ai`
- **Image Handler**: `open-sse/handlers/imageProviders/blackForestLabs.js`

## 2. Endpoints & Async Polling Protocol
- **Base URL**: `https://api.bfl.ai/v1`
- **Submit Endpoint**: `POST https://api.bfl.ai/v1/{model}`
- **Authentication Header**: `x-key: <apiKey || accessToken>`
- **Submit Request Body**:
  ```json
  {
    "prompt": "<prompt>",
    "width": 1024,
    "height": 1024,
    "image_prompt": "<optional_input_image>"
  }
  ```
  (`width` and `height` are parsed by splitting `body.size` on `"x"`).
- **Polling Protocol**:
  1. Initial POST returns `{ "polling_url": "<url>" }`.
  2. Loops every `POLL_INTERVAL_MS` up to `POLL_TIMEOUT_MS`, calling `GET <polling_url>` with headers `{ "x-key": "<key>", "Accept": "application/json" }`.
  3. When `s.status === "Ready"`, extracts `s.result.sample` URL and normalizes to OpenAI image format:
     ```json
     {
       "created": 1700000000,
       "data": [{ "url": "<result.sample>" }]
     }
     ```
  4. When `s.status === "Error"` or `"Failed"`, throws `s.error || "BFL generation failed"`.

## 3. Supported Models
- `flux-pro-1.1` ("FLUX Pro 1.1", `params: ["n", "size"]`, `kind: "image"`)
- `flux-pro-1.1-ultra` ("FLUX Pro 1.1 Ultra", `params: ["size"]`, `kind: "image"`)
- `flux-pro` ("FLUX Pro", `params: ["n", "size"]`, `kind: "image"`)
- `flux-dev` ("FLUX Dev", `params: ["n", "size"]`, `kind: "image"`)
- `flux-kontext-pro` ("FLUX Kontext Pro (Edit)", `params: ["size"]`, `capabilities: ["edit"]`, `kind: "image"`)
- `flux-kontext-max` ("FLUX Kontext Max (Edit)", `params: ["size"]`, `capabilities: ["edit"]`, `kind: "image"`)

## 4. Source Implementation

### Registry (`open-sse/providers/registry/black-forest-labs.js`)
```javascript
export default {
  id: "black-forest-labs",
  priority: 50,
  alias: "black-forest-labs",
  aliases: [
    "bfl",
  ],
  uiAlias: "bfl",
  display: {
    name: "Black Forest Labs",
    icon: "image",
    color: "#111827",
    textIcon: "BF",
    website: "https://blackforestlabs.ai",
    notice: {
      apiKeyUrl: "https://api.bfl.ai",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: null,
  models: [
    { id: "flux-pro-1.1", name: "FLUX Pro 1.1", params: ["n","size"], kind: "image" },
    { id: "flux-pro-1.1-ultra", name: "FLUX Pro 1.1 Ultra", params: ["size"], kind: "image" },
    { id: "flux-pro", name: "FLUX Pro", params: ["n","size"], kind: "image" },
    { id: "flux-dev", name: "FLUX Dev", params: ["n","size"], kind: "image" },
    { id: "flux-kontext-pro", name: "FLUX Kontext Pro (Edit)", params: ["size"], capabilities: ["edit"], kind: "image" },
    { id: "flux-kontext-max", name: "FLUX Kontext Max (Edit)", params: ["size"], capabilities: ["edit"], kind: "image" },
  ],
  serviceKinds: ["image"],
  imageConfig: { baseUrl: "https://api.bfl.ai/v1" },
};
```

### Image Provider Adapter (`open-sse/handlers/imageProviders/blackForestLabs.js`)
```javascript
// Black Forest Labs (FLUX) — async submit + polling_url
import { sleep, nowSec, POLL_INTERVAL_MS, POLL_TIMEOUT_MS } from "./_base.js";
import { PROVIDER_MEDIA } from "../../providers/index.js";

const BASE_URL = PROVIDER_MEDIA["black-forest-labs"]?.imageConfig?.baseUrl;

export default {
  async: true,
  buildUrl: (model) => `${BASE_URL}/${model}`,
  buildHeaders: (creds) => {
    const key = creds?.apiKey || creds?.accessToken;
    return { "Content-Type": "application/json", "x-key": key };
  },
  buildBody: (_model, body) => {
    const req = { prompt: body.prompt };
    if (body.size) {
      const [w, h] = body.size.split("x").map(Number);
      if (w) req.width = w;
      if (h) req.height = h;
    }
    if (body.image) req.image_prompt = body.image;
    return req;
  },
  async parseResponse(response, { headers }) {
    const data = await response.json();
    const pollingUrl = data.polling_url;
    if (!pollingUrl) throw new Error("BFL: no polling_url returned");
    const deadline = Date.now() + POLL_TIMEOUT_MS;
    while (Date.now() < deadline) {
      await sleep(POLL_INTERVAL_MS);
      const r = await fetch(pollingUrl, { headers: { "x-key": headers["x-key"], "Accept": "application/json" } });
      if (!r.ok) throw new Error(`BFL status ${r.status}`);
      const s = await r.json();
      if (s.status === "Ready") return s;
      if (s.status === "Error" || s.status === "Failed") throw new Error(s.error || "BFL generation failed");
    }
    throw new Error("BFL polling timeout");
  },
  normalize: (responseBody) => {
    const sample = responseBody.result?.sample;
    if (sample) return { created: nowSec(), data: [{ url: sample }] };
    return { created: nowSec(), data: [] };
  },
};
```
