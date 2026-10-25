# Ollama Cloud (`ollama`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `ollama`
- **Priority**: `30` (`hasFree: true`)
- **Alias**: `ollama`
- **Display Name**: Ollama Cloud
- **Category**: `freeTier`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`)
- **Service Kinds**: `["llm", "webFetch", "systemone"]`
- **Website**: `https://ollama.com`
- **API Key URL**: `https://ollama.com/settings/keys`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/ollama.js`
  - `open-sse/handlers/fetch/index.js` (`runOllama`)
  - `open-sse/handlers/systemoneCore.js` (`handleSystemoneCore`)

---

## 2. Endpoints by Service Kind
1. **LLM Chat Endpoint**: `POST https://ollama.com/api/chat` (`format: "ollama"`, NDJSON streaming format)
2. **Validation / Tags Endpoint**: `GET https://ollama.com/api/tags`
3. **System One Decision Endpoint (`systemoneConfig`)**: `POST https://ollama.com/v1/systemone` (`Authorization: Bearer <apiKey>`)
4. **Web Fetch Endpoint (`fetchConfig` / `runOllama`)**:
   - `POST https://ollama.com/api/web_fetch`
   - Headers: `content-type: application/json`, `authorization: Bearer <apiKey>`
   - Body: `{ "url": "<url>" }`
   - Response: `{ title, content, links }` (truncated to `maxCharacters: 200000`, `timeoutMs: 30000`).

---

## 3. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `gpt-oss:120b` | GPT OSS 120B | `llm` |
| `kimi-k2.5` | Kimi K2.5 | `llm` |
| `glm-5` | GLM 5 | `llm` |
| `minimax-m2.5` | MiniMax M2.5 | `llm` |
| `glm-4.7-flash` | GLM 4.7 Flash | `llm` |
| `qwen3.5` | Qwen3.5 | `llm` |
| `minimax-m3` | MiniMax M3 | `llm` |
| `deepseek-v4.1-flash:cloud` | DeepSeek V4.1 Flash | `llm` |
| `clef` | Clef | `systemone` |
| `clef-flash` | Clef Flash | `systemone` |
| `clef:cloud` | Clef (Cloud) | `systemone` |

---

## 4. Verbatim Source Code

### `open-sse/providers/registry/ollama.js`
```javascript
export default {
  id: "ollama",
  priority: 30,
  hasFree: true,
  alias: "ollama",
  display: {
    name: "Ollama Cloud",
    icon: "cloud",
    color: "#ffffffff",
    textIcon: "OL",
    website: "https://ollama.com",
    notice: {
      text: "Free tier: light usage, 1 cloud model at a time (limits reset every 5h & 7d). Pro $20/mo · Max $100/mo.",
      apiKeyUrl: "https://ollama.com/settings/keys",
    },
  },
  category: "freeTier",
  authType: "apikey",
  authModes: ["apikey"],
  transport: {
    baseUrl: "https://ollama.com/api/chat",
    validateUrl: "https://ollama.com/api/tags",
    format: "ollama",
  },
  models: [
    { id: "gpt-oss:120b", name: "GPT OSS 120B" },
    { id: "kimi-k2.5", name: "Kimi K2.5" },
    { id: "glm-5", name: "GLM 5" },
    { id: "minimax-m2.5", name: "MiniMax M2.5" },
    { id: "glm-4.7-flash", name: "GLM 4.7 Flash" },
    { id: "qwen3.5", name: "Qwen3.5" },
    { id: "minimax-m3", name: "MiniMax M3" },
    { id: "deepseek-v4.1-flash:cloud", name: "DeepSeek V4.1 Flash" },
    { id: "clef", name: "Clef", kind: "systemone" },
    { id: "clef-flash", name: "Clef Flash", kind: "systemone" },
    { id: "clef:cloud", name: "Clef (Cloud)", kind: "systemone" },
  ],
  serviceKinds: ["llm", "webFetch", "systemone"],
  systemoneConfig: {
    baseUrl: "https://ollama.com/v1/systemone",
    authType: "apikey",
    authHeader: "bearer",
  },
  fetchConfig: {
    baseUrl: "https://ollama.com/api/web_fetch",
    method: "POST",
    authType: "apikey",
    authHeader: "bearer",
    formats: ["markdown"],
    maxCharacters: 200000,
    timeoutMs: 30000,
  },
  features: {
    usage: true,
    usageApikey: true,
  },
};
```
