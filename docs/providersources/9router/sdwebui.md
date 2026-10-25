# SD WebUI (`sdwebui`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `sdwebui`
- **Priority**: `110`
- **Alias**: `sdwebui`
- **Display Name**: SD WebUI (AUTOMATIC1111)
- **Category**: `apikey` (media handler sets `noAuth: true`)
- **Service Kinds**: `["image"]`
- **Website**: `https://github.com/AUTOMATIC1111/stable-diffusion-webui`
- **Source Files**:
  - `open-sse/providers/registry/sdwebui.js`
  - `open-sse/handlers/imageProviders/sdwebui.js`

---

## 2. Endpoints & Request/Response Format (`sdwebui.js`)
- **Endpoint**: `POST http://localhost:7860/sdapi/v1/txt2img`
- **Headers**: `Content-Type: application/json` (`noAuth: true`)
- **Request Payload (`buildBody`)**:
  ```json
  {
    "prompt": "<prompt>",
    "width": 1024,
    "height": 1024,
    "steps": 20,
    "batch_size": 1
  }
  ```
  - Parses `size` (default `"1024x1024"`) on `"x"` into `[width, height]` (defaulting to `512` if invalid).
- **Response Normalization (`normalize`)**:
  - Maps `responseBody.images[]` (base64 strings) into `{ created, data: [{ b64_json: img }] }`.

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `stable-diffusion-v1-5` | Stable Diffusion v1.5 | `image` | `["n", "size"]` |
| `sdxl-base-1.0` | SDXL Base 1.0 | `image` | `["n", "size"]` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/sdwebui.js`
```javascript
export default {
  id: "sdwebui",
  priority: 110,
  alias: "sdwebui",
  display: {
    name: "SD WebUI",
    icon: "brush",
    color: "#FF7043",
    textIcon: "SD",
    website: "https://github.com/AUTOMATIC1111/stable-diffusion-webui",
  },
  category: "apikey",
  transport: null,
  models: [
    { id: "stable-diffusion-v1-5", name: "Stable Diffusion v1.5", params: ["n","size"], kind: "image" },
    { id: "sdxl-base-1.0", name: "SDXL Base 1.0", params: ["n","size"], kind: "image" },
  ],
  serviceKinds: ["image"],
  imageConfig: { baseUrl: "http://localhost:7860/sdapi/v1/txt2img" },
};
```

### `open-sse/handlers/imageProviders/sdwebui.js`
```javascript
// SD WebUI (AUTOMATIC1111) — local, noAuth
import { nowSec } from "./_base.js";
import { PROVIDER_MEDIA } from "../../providers/index.js";

const BASE_URL = PROVIDER_MEDIA["sdwebui"]?.imageConfig?.baseUrl;

export default {
  noAuth: true,
  buildUrl: () => BASE_URL,
  buildHeaders: () => ({ "Content-Type": "application/json" }),
  buildBody: (_model, body) => {
    const { prompt, n = 1, size = "1024x1024" } = body;
    const [width, height] = size.split("x").map(Number);
    return { prompt, width: width || 512, height: height || 512, steps: 20, batch_size: n };
  },
  normalize: (responseBody) => {
    const images = Array.isArray(responseBody.images) ? responseBody.images.map((img) => ({ b64_json: img })) : [];
    return { created: nowSec(), data: images };
  },
};
```
