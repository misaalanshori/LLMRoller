# Runway ML (`runwayml`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `runwayml`
- **Priority**: `80`
- **Primary Alias**: `runwayml`
- **Additional Aliases**: `["runway"]`
- **UI Alias**: `runway`
- **Display Name**: Runway ML
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["image"]` (also includes `kind: "video"` models in `models[]`)
- **Website**: `https://runwayml.com`
- **API Key URL**: `https://dev.runwayml.com`
- **Source Files**:
  - `open-sse/providers/registry/runwayml.js`
  - `open-sse/handlers/imageProviders/runwayml.js`

---

## 2. Endpoints & Async Task Polling Flow (`runwayml.js`)
- **Base URL**: `https://api.dev.runwayml.com/v1`
- **Submit Endpoints**:
  - Image Models (`model.includes("image")`): `POST https://api.dev.runwayml.com/v1/text_to_image`
  - Video Models (`!model.includes("image")`): `POST https://api.dev.runwayml.com/v1/image_to_video`
- **Task Polling Endpoint**: `GET https://api.dev.runwayml.com/v1/tasks/<id>`
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
  - `X-Runway-Version: 2024-11-06`
- **Request Payload (`buildBody`)**:
  - Image: `{ promptText: body.prompt, model, ratio, ...(body.image ? { referenceImages: [{ uri: body.image }] } : {}) }`
  - Video: `{ promptText: body.prompt, model, ratio, duration: 5, ...(body.image ? { promptImage: body.image } : {}) }`
- **Polling & Normalization**:
  - Polls `/tasks/<id>` every `2000ms` until `s.status === "SUCCEEDED"` (throws on `"FAILED"` or `"CANCELLED"`).
  - Maps `responseBody.output[]` to `{ created, data: outputs.map((url) => ({ url })) }`.

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `gen4_image` | Gen-4 Image | `image` | `["size"]` |
| `gen4_image_turbo` | Gen-4 Image Turbo | `image` | `["size"]` |
| `gen4_turbo` | Gen-4 Turbo | `video` | `[]` |
| `gen3a_turbo` | Gen-3 Alpha Turbo | `video` | `[]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/handlers/imageProviders/runwayml.js`
```javascript
import { sleep, nowSec, sizeToAspectRatio, POLL_INTERVAL_MS, POLL_TIMEOUT_MS } from "./_base.js";
import { PROVIDER_MEDIA } from "../../providers/index.js";

const BASE_URL = PROVIDER_MEDIA["runwayml"]?.imageConfig?.baseUrl;

export default {
  async: true,
  buildUrl: (model) => {
    return `${BASE_URL}/${model.includes("image") ? "text_to_image" : "image_to_video"}`;
  },
  buildHeaders: (creds) => {
    const key = creds?.apiKey || creds?.accessToken;
    return {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${key}`,
      "X-Runway-Version": "2024-11-06",
    };
  },
  buildBody: (model, body) => {
    const isVideo = !model.includes("image");
    const ratio = sizeToAspectRatio(body.size);
    if (isVideo) {
      return { promptText: body.prompt, model, ratio, duration: 5, ...(body.image ? { promptImage: body.image } : {}) };
    }
    return { promptText: body.prompt, model, ratio, ...(body.image ? { referenceImages: [{ uri: body.image }] } : {}) };
  },
  async parseResponse(response, { headers }) {
    const { id } = await response.json();
    if (!id) throw new Error("Runway: no task id returned");
    const taskUrl = `${BASE_URL}/tasks/${id}`;
    const deadline = Date.now() + POLL_TIMEOUT_MS;
    while (Date.now() < deadline) {
      await sleep(POLL_INTERVAL_MS);
      const r = await fetch(taskUrl, { headers });
      if (!r.ok) throw new Error(`Runway status ${r.status}`);
      const s = await r.json();
      if (s.status === "SUCCEEDED") return s;
      if (s.status === "FAILED" || s.status === "CANCELLED") throw new Error(s.failure || "Runway task failed");
    }
    throw new Error("Runway polling timeout");
  },
  normalize: (responseBody) => {
    const outputs = Array.isArray(responseBody.output) ? responseBody.output : [];
    return { created: nowSec(), data: outputs.map((url) => ({ url })) };
  },
};
```
