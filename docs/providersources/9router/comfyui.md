# ComfyUI (`comfyui`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `comfyui`
- **Priority**: `120`
- **Alias**: `comfyui`
- **Display Name**: ComfyUI
- **Category**: `apikey` (media handler sets `noAuth: true`)
- **Service Kinds**: `["image"]`
- **Website**: `https://github.com/comfyanonymous/ComfyUI`
- **Source Files**:
  - `open-sse/providers/registry/comfyui.js`
  - `open-sse/handlers/imageProviders/comfyui.js`

---

## 2. Endpoints & Image Handler
- **Base URL**: `http://localhost:8188` (`imageConfig.baseUrl`)
- **HTTP Method**: `POST http://localhost:8188`
- **Headers**: `Content-Type: application/json` (`noAuth: true`)
- **Request Body**: `{ "prompt": body.prompt }`
- **Response Normalization**: Identity pass-through (`(responseBody) => responseBody`)

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `flux-dev` | FLUX Dev | `image` | `["n", "size"]` |
| `sdxl` | SDXL | `image` | `["n", "size"]` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/comfyui.js`
```javascript
export default {
  id: "comfyui",
  priority: 120,
  alias: "comfyui",
  display: {
    name: "ComfyUI",
    icon: "account_tree",
    color: "#4CAF50",
    textIcon: "CF",
    website: "https://github.com/comfyanonymous/ComfyUI",
  },
  category: "apikey",
  transport: null,
  models: [
    { id: "flux-dev", name: "FLUX Dev", params: ["n","size"], kind: "image" },
    { id: "sdxl", name: "SDXL", params: ["n","size"], kind: "image" },
  ],
  serviceKinds: ["image"],
  imageConfig: { baseUrl: "http://localhost:8188" },
};
```

### `open-sse/handlers/imageProviders/comfyui.js`
```javascript
// ComfyUI — local, noAuth (placeholder; full graph workflow not implemented)
import { PROVIDER_MEDIA } from "../../providers/index.js";

const BASE_URL = PROVIDER_MEDIA["comfyui"]?.imageConfig?.baseUrl;

export default {
  noAuth: true,
  buildUrl: () => BASE_URL,
  buildHeaders: () => ({ "Content-Type": "application/json" }),
  buildBody: (_model, body) => ({ prompt: body.prompt }),
  normalize: (responseBody) => responseBody,
};
```
