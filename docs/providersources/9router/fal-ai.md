# Fal.ai (`fal-ai`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `fal-ai`
- **Priority**: `90` (`hasFree: true`)
- **Primary Alias**: `fal-ai`
- **Additional Aliases**: `["fal"]`
- **UI Alias**: `fal`
- **Display Name**: Fal.ai
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["image"]`
- **Website**: `https://fal.ai`
- **API Key URL**: `https://fal.ai/dashboard/keys`
- **Source Files**:
  - `open-sse/providers/registry/fal-ai.js`
  - `open-sse/handlers/imageProviders/falAi.js`

---

## 2. Endpoints & Asynchronous Queue Polling Flow
- **Queue Submit Endpoint**: `POST https://queue.fal.run/<model>`
- **Headers**:
  - `Authorization`: `Key <apiKey>`
  - `Content-Type`: `application/json`
- **Submit Request Body (`buildBody`)**:
  ```json
  {
    "prompt": "<prompt>",
    "num_images": 1,
    "image_size": "<sizeToAspectRatio(size)>",
    "image_url": "<body.image>"
  }
  ```
- **Queue Polling (`parseResponse`)**:
  - Parses `{ status_url, response_url }` from the initial queue submission response.
  - Polls `GET <status_url>` every `POLL_INTERVAL_MS` (`2000ms`) up to `POLL_TIMEOUT_MS` (`120000ms`) with the same `Authorization: Key <apiKey>` header.
  - When `s.status === "COMPLETED"`, fetches `GET <response_url>` and normalizes `responseBody.images` (or `[responseBody.image]`) into `{ created, data: [{ url: img.url || img }] }`.
  - When `s.status === "FAILED"`, throws `new Error(s.error || "Fal generation failed")`.

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `fal-ai/flux/schnell` | FLUX Schnell | `image` | `["n", "size"]` |
| `fal-ai/flux/dev` | FLUX Dev | `image` | `["n", "size"]` |
| `fal-ai/flux-pro/v1.1` | FLUX Pro v1.1 | `image` | `["n", "size"]` |
| `fal-ai/flux-pro/v1.1-ultra` | FLUX Pro v1.1 Ultra | `image` | `["n", "size"]` |
| `fal-ai/recraft-v3` | Recraft V3 | `image` | `["n", "size", "style"]` |
| `fal-ai/ideogram/v2` | Ideogram V2 | `image` | `["n", "size", "style"]` |
| `fal-ai/stable-diffusion-v35-large` | SD 3.5 Large | `image` | `["n", "size"]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/fal-ai.js`
```javascript
export default {
  id: "fal-ai",
  priority: 90,
  hasFree: true,
  alias: "fal-ai",
  aliases: [
    "fal",
  ],
  uiAlias: "fal",
  display: {
    name: "Fal.ai",
    icon: "image",
    color: "#2563EB",
    textIcon: "FL",
    website: "https://fal.ai",
    notice: {
      apiKeyUrl: "https://fal.ai/dashboard/keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: null,
  models: [
    { id: "fal-ai/flux/schnell", name: "FLUX Schnell", params: ["n","size"], kind: "image" },
    { id: "fal-ai/flux/dev", name: "FLUX Dev", params: ["n","size"], kind: "image" },
    { id: "fal-ai/flux-pro/v1.1", name: "FLUX Pro v1.1", params: ["n","size"], kind: "image" },
    { id: "fal-ai/flux-pro/v1.1-ultra", name: "FLUX Pro v1.1 Ultra", params: ["n","size"], kind: "image" },
    { id: "fal-ai/recraft-v3", name: "Recraft V3", params: ["n","size","style"], kind: "image" },
    { id: "fal-ai/ideogram/v2", name: "Ideogram V2", params: ["n","size","style"], kind: "image" },
    { id: "fal-ai/stable-diffusion-v35-large", name: "SD 3.5 Large", params: ["n","size"], kind: "image" },
  ],
  serviceKinds: ["image"],
  imageConfig: { baseUrl: "https://queue.fal.run" },
};
```

### `open-sse/handlers/imageProviders/falAi.js`
```javascript
// Fal.ai — async submit + queue polling
import { sleep, nowSec, sizeToAspectRatio, POLL_INTERVAL_MS, POLL_TIMEOUT_MS } from "./_base.js";
import { PROVIDER_MEDIA } from "../../providers/index.js";

const BASE_URL = PROVIDER_MEDIA["fal-ai"]?.imageConfig?.baseUrl;

export default {
  async: true,
  buildUrl: (model) => `${BASE_URL}/${model}`,
  buildHeaders: (creds) => {
    const key = creds?.apiKey || creds?.accessToken;
    return { "Content-Type": "application/json", "Authorization": `Key ${key}` };
  },
  buildBody: (_model, body) => {
    const req = { prompt: body.prompt, num_images: body.n || 1 };
    if (body.size) req.image_size = sizeToAspectRatio(body.size);
    if (body.image) req.image_url = body.image;
    return req;
  },
  async parseResponse(response, { headers }) {
    const { status_url, response_url } = await response.json();
    const deadline = Date.now() + POLL_TIMEOUT_MS;
    while (Date.now() < deadline) {
      await sleep(POLL_INTERVAL_MS);
      const r = await fetch(status_url, { headers });
      if (!r.ok) throw new Error(`Fal status ${r.status}`);
      const s = await r.json();
      if (s.status === "COMPLETED") {
        const fr = await fetch(response_url, { headers });
        return await fr.json();
      }
      if (s.status === "FAILED") throw new Error(s.error || "Fal generation failed");
    }
    throw new Error("Fal polling timeout");
  },
  normalize: (responseBody) => {
    const images = Array.isArray(responseBody.images)
      ? responseBody.images
      : (responseBody.image ? [responseBody.image] : []);
    return { created: nowSec(), data: images.map((img) => ({ url: img.url || img })) };
  },
};
```
