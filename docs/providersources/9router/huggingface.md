# HuggingFace (`huggingface`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `huggingface`
- **Priority**: `70` (`hasFree: true`)
- **Primary Alias**: `huggingface`
- **Additional Aliases**: `["hf"]`
- **UI Alias**: `hf`
- **Display Name**: HuggingFace
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["image", "stt"]` (`hiddenKinds: ["tts"]`)
- **Website**: `https://huggingface.co`
- **API Key URL**: `https://huggingface.co/settings/tokens`
- **Source Files**:
  - `open-sse/providers/registry/huggingface.js`
  - `open-sse/handlers/imageProviders/huggingface.js`
  - `open-sse/handlers/sttCore.js` (`transcribeHuggingFace`)
  - `open-sse/handlers/ttsProviders/genericFormats.js` (`huggingface` TTS handler)

---

## 2. Endpoints & Inference Providers Router Architecture
1. **Image Generation & Image-to-Image (`open-sse/handlers/imageProviders/huggingface.js`)**:
   - **Base URL**: `https://router.huggingface.co` (or custom `credentials.providerSpecificData.baseUrl` for self-hosted TGI).
   - **URL Resolution**: Resolves Hub model ID through `imageConfig.modelMap` to `https://router.huggingface.co/<provider>/<providerModelId>` (primarily `fal-ai/...` routes).
   - **Headers**: `Authorization: Bearer <apiKey>`, `Content-Type: application/json`.
   - **Text-to-Image Payload**: `{ "inputs": "<prompt>" }`.
   - **Image-to-Image Payload (`task === "image-to-image"`)**: `{ "inputs": "<raw_base64_source_image>", "parameters": { "prompt": "<prompt>" } }`.
   - **Response Parsing**: Reads raw binary image bytes (`response.arrayBuffer()`) and encodes as `{ created, data: [{ b64_json: base64 }] }`.
2. **Speech-to-Text (`transcribeHuggingFace` in `open-sse/handlers/sttCore.js`)**:
   - **Endpoint**: `POST https://router.huggingface.co/hf-inference/models/<model>`
   - **Headers**: `Authorization: Bearer <apiKey>`, `Content-Type: <audio_mime_type>`
   - **Body**: Raw binary `ArrayBuffer` of the audio file.
   - **Response**: `{ text: data.text || "" }`.

---

## 3. Supported Models & Router Mapping
| Hub Model ID | Router Path (`imageConfig.modelMap`) | Task / Kind |
|---|---|---|
| `black-forest-labs/FLUX.1-schnell` | `fal-ai/fal-ai/flux/schnell` | `text-to-image` |
| `black-forest-labs/FLUX.1-dev` | `fal-ai/fal-ai/flux/dev` | `text-to-image` |
| `black-forest-labs/FLUX.1-Krea-dev` | `fal-ai/fal-ai/flux/krea` | `text-to-image` |
| `black-forest-labs/FLUX.1-Kontext-dev` | `fal-ai/fal-ai/flux-kontext/dev` | `image-to-image` |
| `black-forest-labs/FLUX.2-dev` | `fal-ai/fal-ai/flux-2/edit` | `image-to-image` |
| `black-forest-labs/FLUX.2-klein-9B` | `fal-ai/fal-ai/flux-2/klein/9b/edit` | `image-to-image` |
| `black-forest-labs/FLUX.2-klein-4B` | `fal-ai/fal-ai/flux-2/klein/4b/distilled/edit` | `image-to-image` |
| `black-forest-labs/FLUX.2-klein-base-9B` | `fal-ai/fal-ai/flux-2/klein/9b/base/edit` | `image-to-image` |
| `black-forest-labs/FLUX.2-klein-base-4B` | `fal-ai/fal-ai/flux-2/klein/4b/base/edit` | `image-to-image` |
| `stabilityai/stable-diffusion-xl-base-1.0` | `fal-ai/fal-ai/fast-sdxl` | `text-to-image` |
| `stabilityai/stable-diffusion-3.5-large` | `fal-ai/fal-ai/stable-diffusion-v35-large` | `text-to-image` |
| `stabilityai/stable-diffusion-3.5-large-turbo` | `fal-ai/fal-ai/stable-diffusion-v35-large/turbo` | `text-to-image` |
| `Qwen/Qwen-Image` | `fal-ai/fal-ai/qwen-image` | `text-to-image` |
| `Qwen/Qwen-Image-2512` | `fal-ai/fal-ai/qwen-image-2512` | `text-to-image` |
| `Qwen/Qwen-Image-Edit` | `fal-ai/fal-ai/qwen-image-edit` | `image-to-image` |
| `Qwen/Qwen-Image-Edit-2509` | `fal-ai/fal-ai/qwen-image-edit-2509` | `image-to-image` |
| `Qwen/Qwen-Image-Edit-2511` | `fal-ai/fal-ai/qwen-image-edit-plus` | `image-to-image` |
| `ideogram-ai/ideogram-4-fp8` | `fal-ai/ideogram/v4` | `text-to-image` |
| `tencent/HunyuanImage-3.0` | `fal-ai/fal-ai/hunyuan-image/v3/text-to-image` | `text-to-image` |
| `Tongyi-MAI/Z-Image-Turbo` | `fal-ai/fal-ai/z-image/turbo` | `text-to-image` |
| `krea/Krea-2-Turbo` | `fal-ai/fal-ai/krea-2/turbo` | `text-to-image` |
| `HiDream-ai/HiDream-I1-Fast` | `fal-ai/fal-ai/hidream-i1-fast` | `text-to-image` |
| `playgroundai/playground-v2.5-1024px-aesthetic` | `fal-ai/fal-ai/playground-v25` | `text-to-image` |
| `openai/whisper-large-v3` | `/hf-inference/models/openai/whisper-large-v3` | `stt` |
| `openai/whisper-large-v3-turbo` | `/hf-inference/models/openai/whisper-large-v3-turbo` | `stt` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/handlers/imageProviders/huggingface.js`
```javascript
export default {
  buildUrl: (model, creds) => {
    const override = customBaseUrl(creds);
    if (override) {
      if (model.includes("..") || model.includes("//") || /[?#]/.test(model)) {
        throw new Error(`HuggingFace: invalid model ID "${model}"`);
      }
      return `${override}/${model}`;
    }

    const entry = lookup(model);
    if (!entry) {
      throw new Error(
        `HuggingFace: no HuggingFace router mapping for model "${model}". ` +
          `Add it to imageConfig.modelMap in open-sse/providers/registry/huggingface.js, ` +
          `or set a custom base URL on the connection.`
      );
    }
    return `${BASE_URL}/${mappingPath(entry)}`;
  },
  buildHeaders: (creds) => {
    const headers = { "Content-Type": "application/json" };
    const key = creds?.apiKey || creds?.accessToken;
    if (key) headers["Authorization"] = `Bearer ${key}`;
    return headers;
  },
  buildBody: async (model, body) => {
    const entry = lookup(model);
    const task = mappingTask(entry || "");

    if (task === "image-to-image") {
      const image = await sourceImage(body);
      if (!image) {
        throw new Error(
          `HuggingFace: model "${model}" requires a source image. ` +
            `Send it as "image" (or "images") in the request body.`
        );
      }
      return { inputs: image, parameters: { prompt: body.prompt } };
    }

    return { inputs: body.prompt };
  },
  async parseResponse(response) {
    const buf = await response.arrayBuffer();
    const base64 = Buffer.from(buf).toString("base64");
    return { created: nowSec(), data: [{ b64_json: base64 }] };
  },
  normalize: (responseBody) => responseBody,
};
```
