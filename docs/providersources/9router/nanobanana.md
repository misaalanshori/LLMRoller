# NanoBanana API (`nanobanana`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `nanobanana`
- **Priority**: `80` (`hasFree: true`)
- **Primary Alias**: `nanobanana`
- **Additional Aliases**: `["nb"]`
- **UI Alias**: `nb`
- **Display Name**: NanoBanana API
- **Category**: `apikey`
- **Service Kinds**: `["image"]`
- **Website**: `https://nanobananaapi.ai`
- **API Key URL**: `https://nanobananaapi.ai/dashboard`
- **Source Files**:
  - `open-sse/providers/registry/nanobanana.js`
  - `open-sse/handlers/imageProviders/nanobanana.js`

---

## 2. Endpoints & Async Task Polling Flow (`nanobanana.js`)
- **Submit Endpoint**: `POST https://api.nanobananaapi.ai/api/v1/nanobanana/generate`
- **Poll Endpoint**: `GET https://api.nanobananaapi.ai/api/v1/nanobanana/record-info?taskId=<taskId>`
- **Validation Endpoint**: `GET https://api.nanobananaapi.ai/v1/models`
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
- **Request Payload (`buildBody`)**:
  ```json
  {
    "prompt": "<prompt>",
    "type": "TEXTTOIAMGE",
    "numImages": 1,
    "image_size": "<aspect_ratio>",
    "callBackUrl": "https://localhost/callback",
    "imageUrls": ["..."]
  }
  ```
  - Note: Uses NanoBanana's literal spelling `"TEXTTOIAMGE"` (or `"IMAGETOIAMGE"` when `body.image` / `body.images` is present, populating `imageUrls`).
- **Response Polling (`parseResponse` & `normalize`)**:
  - Checks `submitData.code === 200` and extracts `submitData.data.taskId`.
  - Polls `pollUrl` every `2000ms` until `s.data.successFlag === 1` (throws on `2` or `3`), then extracts `responseBody.response.resultImageUrl || responseBody.response.originImageUrl`.

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `nanobanana-flash` | NanoBanana Flash | `image` | `["n", "size"]` |
| `nanobanana-pro` | NanoBanana Pro | `image` | `["n", "size"]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/handlers/imageProviders/nanobanana.js`
```javascript
import { sleep, nowSec, sizeToAspectRatio, POLL_INTERVAL_MS, POLL_TIMEOUT_MS } from "./_base.js";
import { PROVIDER_MEDIA } from "../../providers/index.js";

const IMG_CFG = PROVIDER_MEDIA["nanobanana"]?.imageConfig || {};
const SUBMIT_URL = IMG_CFG.baseUrl;
const POLL_BASE = IMG_CFG.pollUrl;

export default {
  async: true,
  buildUrl: () => SUBMIT_URL,
  buildHeaders: (creds) => {
    const headers = { "Content-Type": "application/json" };
    const key = creds?.apiKey || creds?.accessToken;
    if (key) headers["Authorization"] = `Bearer ${key}`;
    return headers;
  },
  buildBody: (_model, body) => {
    const ratio = sizeToAspectRatio(body.size);
    const isEdit = !!(body.image || (Array.isArray(body.images) && body.images.length));
    const req = {
      prompt: body.prompt,
      type: isEdit ? "IMAGETOIAMGE" : "TEXTTOIAMGE",
      numImages: body.n || 1,
      image_size: ratio,
      callBackUrl: "https://localhost/callback",
    };
    if (isEdit) {
      const urls = Array.isArray(body.images) ? body.images.filter(Boolean) : [];
      if (body.image) urls.push(body.image);
      req.imageUrls = urls;
    }
    return req;
  },
  async parseResponse(response, { headers }) {
    const submitData = await response.json();
    if (submitData.code !== 200) throw new Error(submitData.msg || "NanoBanana submit failed");
    const taskId = submitData.data?.taskId;
    if (!taskId) throw new Error("NanoBanana: no taskId returned");
    const pollUrl = `${POLL_BASE}?taskId=${encodeURIComponent(taskId)}`;
    const deadline = Date.now() + POLL_TIMEOUT_MS;
    while (Date.now() < deadline) {
      await sleep(POLL_INTERVAL_MS);
      const r = await fetch(pollUrl, { headers });
      if (!r.ok) throw new Error(`NanoBanana status ${r.status}`);
      const s = await r.json();
      const flag = s.data?.successFlag;
      if (flag === 1) return s.data;
      if (flag === 2 || flag === 3) throw new Error(s.data?.errorMessage || "NanoBanana generation failed");
    }
    throw new Error("NanoBanana polling timeout");
  },
  normalize: (responseBody, prompt) => {
    const url = responseBody.response?.resultImageUrl || responseBody.response?.originImageUrl;
    if (url) return { created: nowSec(), data: [{ url, revised_prompt: prompt }] };
    return { created: nowSec(), data: [] };
  },
};
```
