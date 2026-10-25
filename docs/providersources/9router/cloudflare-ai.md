# Cloudflare Workers AI (`cloudflare-ai`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `cloudflare-ai`
- **Priority**: `60`
- **Primary Alias**: `cloudflare-ai`
- **Additional Aliases**: `["cf"]`
- **UI Alias**: `cf`
- **Display Name**: Cloudflare
- **Category**: `freeTier` (`hasFree: true`)
- **Auth Type**: `apikey` (`hasProviderSpecificData: true` — requires `accountId` in `credentials.providerSpecificData.accountId`)
- **Service Kinds**: `["llm", "image", "systemone"]`
- **Website**: `https://developers.cloudflare.com/workers-ai/`
- **API Key URL**: `https://dash.cloudflare.com/profile/api-tokens`
- **Source Files**:
  - `open-sse/providers/registry/cloudflare-ai.js`
  - `open-sse/executors/default.js` (URL `{accountId}` interpolation)
  - `open-sse/handlers/imageProviders/cloudflareAi.js`
  - `open-sse/handlers/systemoneCore.js`

---

## 2. Endpoints by Service Kind
1. **LLM Chat Completions**:
   - `POST https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1/chat/completions`
   - `DefaultExecutor.buildUrl` replaces `{accountId}` with `credentials.providerSpecificData.accountId.trim()`.
   - `thinkingFormat`: `"openai"`
2. **Image Generation / Editing / Inpainting (`cloudflareAi.js`)**:
   - `POST https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/{model}`
   - For FLUX.2 models (`@cf/black-forest-labs/flux-2-dev`, `@cf/black-forest-labs/flux-2-klein-4b`, `@cf/black-forest-labs/flux-2-klein-9b`), builds a `multipart/form-data` (`FormData`) payload with `prompt`, `width`, `height`, and optional fields (`negative_prompt`, `guidance`, `seed`, `num_steps`, `steps`, `strength`).
   - For all other image models, sends `application/json` with `{ prompt, width, height, ...optionalFields, image_b64, image: <byteArray>, mask_b64, mask: <byteArray>, mask_image: <byteArray> }`.
   - Parses either raw `image/*` binary response or Cloudflare JSON envelope `{ result: { image: "<base64>" } }`.
3. **System One Decision Models (`systemoneCore.js`)**:
   - `POST https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/{model}`
   - Rewrites `body.model` to `upstreamModelId` (`"clef-flash"` for `@cf/cloudflare/clef-flash`) while keeping `{model}` in the URL path as `@cf/cloudflare/clef-flash`.

---

## 3. Authentication & Headers
- **Authorization Header**: `Authorization: Bearer <apiKey>`
- **Provider-Specific Data**: Requires `credentials.providerSpecificData.accountId` (throws an error if missing).

---

## 4. Supported Models
| Model ID | Display Name | Kind | Extra Metadata |
|---|---|---|---|
| `@cf/cloudflare/clef-flash` | Clef Flash | `systemone` | `upstreamModelId: "clef-flash"` |
| `@cf/meta/llama-3.2-1b-instruct` | Llama 3.2 1B Instruct | `llm` | — |
| `@cf/meta/llama-3.2-3b-instruct` | Llama 3.2 3B Instruct | `llm` | — |
| `@cf/meta/llama-3.1-8b-instruct-fp8-fast` | Llama 3.1 8B Instruct FP8 Fast | `llm` | — |
| `@cf/meta/llama-3.1-8b-instruct-awq` | Llama 3.1 8B Instruct AWQ | `llm` | — |
| `@cf/mistralai/mistral-small-3.1-24b-instruct` | Mistral Small 3.1 24B Instruct | `llm` | — |
| `@cf/meta/llama-3.1-70b-instruct-fp8-fast` | Llama 3.1 70B Instruct FP8 Fast | `llm` | — |
| `@cf/meta/llama-3.3-70b-instruct-fp8-fast` | Llama 3.3 70B Instruct FP8 Fast | `llm` | — |
| `@cf/deepseek-ai/deepseek-r1-distill-qwen-32b` | DeepSeek R1 Distill Qwen 32B | `llm` | — |
| `@cf/moonshotai/kimi-k2.5` | Kimi K2.5 | `llm` | — |
| `@cf/moonshotai/kimi-k2.6` | Kimi K2.6 | `llm` | — |
| `@cf/zai-org/glm-4.7-flash` | GLM 4.7 Flash | `llm` | — |
| `@cf/qwen/qwq-32b` | QwQ 32B | `llm` | — |
| `@cf/qwen/qwen2.5-coder-32b-instruct` | Qwen 2.5 Coder 32B Instruct | `llm` | — |
| `@cf/black-forest-labs/flux-2-klein-9b` | FLUX.2 Klein 9B | `image` | `params: ["size"]` (Multipart) |
| `@cf/black-forest-labs/flux-2-klein-4b` | FLUX.2 Klein 4B | `image` | `params: ["size"]` (Multipart) |
| `@cf/black-forest-labs/flux-2-dev` | FLUX.2 Dev | `image` | `params: ["size"]` (Multipart) |
| `@cf/leonardo/lucid-origin` | Lucid Origin | `image` | `params: ["size"]` |
| `@cf/leonardo/phoenix-1.0` | Phoenix 1.0 | `image` | `params: ["size"]` |
| `@cf/black-forest-labs/flux-1-schnell` | FLUX.1 Schnell | `image` | `params: ["size"]` |
| `@cf/bytedance/stable-diffusion-xl-lightning` | SDXL Lightning | `image` | `params: ["size"]` |
| `@cf/lykon/dreamshaper-8-lcm` | DreamShaper 8 LCM | `image` | `params: ["size"]` |
| `@cf/runwayml/stable-diffusion-v1-5-img2img` | Stable Diffusion v1.5 Img2Img | `image` | `capabilities: ["edit"]` |
| `@cf/runwayml/stable-diffusion-v1-5-inpainting` | Stable Diffusion v1.5 Inpainting | `image` | `capabilities: ["edit", "mask"]` |
| `@cf/stabilityai/stable-diffusion-xl-base-1.0` | SDXL Base 1.0 | `image` | `params: ["size"]` |

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/cloudflare-ai.js`
```javascript
export default {
  id: "cloudflare-ai",
  priority: 60,
  hasFree: true,
  alias: "cloudflare-ai",
  aliases: [
    "cf",
  ],
  uiAlias: "cf",
  display: {
    name: "Cloudflare",
    icon: "cloud",
    color: "#F38020",
    textIcon: "CF",
    website: "https://developers.cloudflare.com/workers-ai/",
    notice: {
      text: "Workers AI free tier. Requires a Cloudflare API token and Account ID.",
      apiKeyUrl: "https://dash.cloudflare.com/profile/api-tokens",
    },
  },
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  hasProviderSpecificData: true,
  transport: {
    baseUrl: "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/v1/chat/completions",
    thinkingFormat: "openai",
  },
  models: [
    { id: "@cf/cloudflare/clef-flash", name: "Clef Flash", kind: "systemone", upstreamModelId: "clef-flash" },
    { id: "@cf/meta/llama-3.2-1b-instruct", name: "Llama 3.2 1B Instruct" },
    { id: "@cf/meta/llama-3.2-3b-instruct", name: "Llama 3.2 3B Instruct" },
    { id: "@cf/meta/llama-3.1-8b-instruct-fp8-fast", name: "Llama 3.1 8B Instruct FP8 Fast" },
    { id: "@cf/meta/llama-3.1-8b-instruct-awq", name: "Llama 3.1 8B Instruct AWQ" },
    { id: "@cf/mistralai/mistral-small-3.1-24b-instruct", name: "Mistral Small 3.1 24B Instruct" },
    { id: "@cf/meta/llama-3.1-70b-instruct-fp8-fast", name: "Llama 3.1 70B Instruct FP8 Fast" },
    { id: "@cf/meta/llama-3.3-70b-instruct-fp8-fast", name: "Llama 3.3 70B Instruct FP8 Fast" },
    { id: "@cf/deepseek-ai/deepseek-r1-distill-qwen-32b", name: "DeepSeek R1 Distill Qwen 32B" },
    { id: "@cf/moonshotai/kimi-k2.5", name: "Kimi K2.5" },
    { id: "@cf/moonshotai/kimi-k2.6", name: "Kimi K2.6" },
    { id: "@cf/zai-org/glm-4.7-flash", name: "GLM 4.7 Flash" },
    { id: "@cf/qwen/qwq-32b", name: "QwQ 32B" },
    { id: "@cf/qwen/qwen2.5-coder-32b-instruct", name: "Qwen 2.5 Coder 32B Instruct" },
    { id: "@cf/black-forest-labs/flux-2-klein-9b", name: "FLUX.2 Klein 9B", params: ["size"], kind: "image" },
    { id: "@cf/black-forest-labs/flux-2-klein-4b", name: "FLUX.2 Klein 4B", params: ["size"], kind: "image" },
    { id: "@cf/black-forest-labs/flux-2-dev", name: "FLUX.2 Dev", params: ["size"], kind: "image" },
    { id: "@cf/leonardo/lucid-origin", name: "Lucid Origin", params: ["size"], kind: "image" },
    { id: "@cf/leonardo/phoenix-1.0", name: "Phoenix 1.0", params: ["size"], kind: "image" },
    { id: "@cf/black-forest-labs/flux-1-schnell", name: "FLUX.1 Schnell", params: ["size"], kind: "image" },
    { id: "@cf/bytedance/stable-diffusion-xl-lightning", name: "SDXL Lightning", params: ["size"], kind: "image" },
    { id: "@cf/lykon/dreamshaper-8-lcm", name: "DreamShaper 8 LCM", params: ["size"], kind: "image" },
    { id: "@cf/runwayml/stable-diffusion-v1-5-img2img", name: "Stable Diffusion v1.5 Img2Img", params: ["size"], capabilities: ["edit"], kind: "image" },
    { id: "@cf/runwayml/stable-diffusion-v1-5-inpainting", name: "Stable Diffusion v1.5 Inpainting", params: ["size"], capabilities: ["edit","mask"], kind: "image" },
    { id: "@cf/stabilityai/stable-diffusion-xl-base-1.0", name: "SDXL Base 1.0", params: ["size"], kind: "image" },
  ],
  serviceKinds: ["llm","image","systemone"],
  systemoneConfig: {
    baseUrl: "https://api.cloudflare.com/client/v4/accounts/{accountId}/ai/run/{model}",
    authType: "apikey",
    authHeader: "bearer",
  },
  imageConfig: { baseUrl: "https://api.cloudflare.com/client/v4/accounts" },
};
```

### `open-sse/handlers/imageProviders/cloudflareAi.js` (Excerpt)
```javascript
export default {
  buildUrl: (model, creds) => {
    const accountId = creds?.providerSpecificData?.accountId;
    if (!accountId) throw new Error("cloudflare-ai requires accountId in providerSpecificData");
    return `${BASE_URL}/${accountId}/ai/run/${model}`;
  },

  buildHeaders: (creds, requestBody) => {
    const headers = {};
    const isMultipart = typeof FormData !== "undefined" && requestBody instanceof FormData;
    if (!isMultipart) {
      headers["Content-Type"] = "application/json";
    }
    const key = creds?.apiKey || creds?.accessToken;
    if (key) headers.Authorization = `Bearer ${key}`;
    return headers;
  },

  buildBody: async (model, body) => (
    MULTIPART_MODELS.has(model)
      ? buildMultipartBody(body)
      : await buildJsonBody(body)
  ),
```
