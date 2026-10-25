# Recraft (`recraft`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `recraft`
- **Priority**: `70`
- **Alias**: `recraft`
- **Display Name**: Recraft
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["image"]`
- **Website**: `https://recraft.ai`
- **API Key URL**: `https://www.recraft.ai/profile/api`
- **Source File**: `open-sse/providers/registry/recraft.js`

---

## 2. Endpoints & Configuration
- **Image Generations Endpoint**: `POST https://external.api.recraft.ai/v1/images/generations` (`imageConfig.baseUrl`)
- **Authentication**: `Authorization: Bearer <apiKey>`
- **Format**: OpenAI-compatible image generation payload (`{ model, prompt, n, size, style }`)

---

## 3. Supported Models
| Model ID | Display Name | Kind | Parameters |
|---|---|---|---|
| `recraftv3` | Recraft V3 | `image` | `["n", "size", "style"]` |
| `recraftv2` | Recraft V2 | `image` | `["n", "size", "style"]` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/recraft.js`
```javascript
export default {
  id: "recraft",
  priority: 70,
  alias: "recraft",
  display: {
    name: "Recraft",
    icon: "image",
    color: "#EC4899",
    textIcon: "RC",
    website: "https://recraft.ai",
    notice: {
      apiKeyUrl: "https://www.recraft.ai/profile/api",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: null,
  models: [
    { id: "recraftv3", name: "Recraft V3", params: ["n","size","style"], kind: "image" },
    { id: "recraftv2", name: "Recraft V2", params: ["n","size","style"], kind: "image" },
  ],
  serviceKinds: ["image"],
  imageConfig: { baseUrl: "https://external.api.recraft.ai/v1/images/generations" },
};
```
