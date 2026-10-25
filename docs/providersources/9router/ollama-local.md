# Ollama Local (`ollama-local`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `ollama-local`
- **Priority**: `50` (`hasFree: true`)
- **Alias**: `ollama-local`
- **Display Name**: Ollama Local
- **Category**: `apikey`
- **Service Kinds**: `["llm", "systemone"]`
- **Website**: `https://ollama.com`
- **Executor**: `OllamaLocalExecutor` (`open-sse/executors/ollama-local.js`)
- **Source Files**:
  - `open-sse/providers/registry/ollama-local.js`
  - `open-sse/executors/ollama-local.js`
  - `open-sse/handlers/systemoneCore.js`

---

## 2. Endpoints & Host Resolution
1. **LLM Chat Endpoint (`OllamaLocalExecutor.buildUrl`)**:
   - Default: `POST http://localhost:11434/api/chat`
   - Dynamic Host Override: `POST ${resolveOllamaLocalHost(credentials)}/api/chat` (resolves `credentials.providerSpecificData.baseUrl` or `OLLAMA_HOST`, stripping trailing `/api/chat` or `/v1`).
   - Wire Format: `"ollama"`
2. **System One Decision Endpoint (`systemoneConfig`)**:
   - Default: `POST http://localhost:11434/v1/systemone`
   - Path Suffix: `"/v1/systemone"` (joined onto custom host override by `buildSystemoneUrl`).

---

## 3. Supported Models
| Model ID | Display Name | Kind |
|---|---|---|
| `clef` | Clef | `systemone` |
| `clef-flash` | Clef Flash | `systemone` |
| *(Any local Ollama model)* | *(Dynamic / user-installed)* | `llm` |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/ollama-local.js`
```javascript
export default {
  id: "ollama-local",
  priority: 50,
  hasFree: true,
  alias: "ollama-local",
  display: {
    name: "Ollama Local",
    icon: "cloud",
    color: "#ffffffff",
    textIcon: "OL",
    website: "https://ollama.com",
  },
  category: "apikey",
  transport: {
    baseUrl: "http://localhost:11434/api/chat",
    format: "ollama",
  },
  models: [
    { id: "clef", name: "Clef", kind: "systemone" },
    { id: "clef-flash", name: "Clef Flash", kind: "systemone" },
  ],
  serviceKinds: ["llm", "systemone"],
  systemoneConfig: {
    baseUrl: "http://localhost:11434/v1/systemone",
    path: "/v1/systemone",
  },
};
```

### `open-sse/executors/ollama-local.js`
```javascript
import { DefaultExecutor } from "./default.js";
import { resolveOllamaLocalHost } from "../config/providers.js";

export class OllamaLocalExecutor extends DefaultExecutor {
  constructor() {
    super("ollama-local");
  }

  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    return `${resolveOllamaLocalHost(credentials)}/api/chat`;
  }
}

export default OllamaLocalExecutor;
```
