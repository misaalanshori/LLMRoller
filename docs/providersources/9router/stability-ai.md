# Stability AI (`stability-ai`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `stability-ai`
- **Display Name**: `Stability AI`
- **Alias**: `stab`
- **Category**: `apikey`
- **Service Kinds**: `["image"]`
- **Handler**: `open-sse/handlers/imageProviders/stabilityAi.js`
- **Website**: `https://platform.stability.ai`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.stability.ai/v2beta/stable-image/generate` |
| **Dynamic Endpoint Suffix** | `/ultra` (if `model.includes("ultra")`), `/sd3` (if `model.includes("sd3")`), `/core` (default) |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Accept Header** | `Accept: application/json` |

---

## 3. Image Handler Implementation (`open-sse/handlers/imageProviders/stabilityAi.js`)

```javascript
// Map model id → endpoint segment
function modelToEndpoint(model) {
  if (model.includes("ultra")) return "ultra";
  if (model.includes("sd3")) return "sd3";
  return "core";
}

export default {
  buildUrl: (model) => `${BASE_URL}/${modelToEndpoint(model)}`,
  buildHeaders: (creds) => {
    const key = creds?.apiKey || creds?.accessToken;
    return {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${key}`,
      "Accept": "application/json",
    };
  },
  buildBody: (model, body) => {
    const req = { prompt: body.prompt, output_format: (body.output_format || "png").toLowerCase() };
    if (body.size) req.aspect_ratio = sizeToAspectRatio(body.size);
    if (body.style) req.style_preset = body.style;
    if (model.includes("sd3")) req.model = model;
    return req;
  },
  normalize: (responseBody) => {
    if (responseBody.image) return { created: nowSec(), data: [{ b64_json: responseBody.image }] };
    return { created: nowSec(), data: [] };
  },
};
```

### Supported Models
- `stable-image-ultra` (`Stable Image Ultra`)
- `stable-image-core` (`Stable Image Core`)
- `sd3.5-large` (`SD 3.5 Large`)
- `sd3.5-large-turbo` (`SD 3.5 Large Turbo`)
- `sd3.5-medium` (`SD 3.5 Medium`)
