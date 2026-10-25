# Kimchi (`kimchi`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `kimchi`
- **Priority**: `95`
- **Alias / UI Alias**: `kimchi`
- **Display Name**: Kimchi (Cast AI)
- **Category**: `freeTier`
- **Auth Modes**: `["oauth", "apikey"]` (`flowType: "browser_token"`)
- **Service Kinds**: `["llm", "imageToText"]`
- **Website**: `https://kimchi.dev` (`https://app.kimchi.dev`)
- **Executor**: `KimchiExecutor` (`open-sse/executors/kimchi.js`)
- **Source Files**:
  - `open-sse/providers/registry/kimchi.js`
  - `open-sse/executors/kimchi.js`
  - `src/lib/oauth/providers/kimchi.js`

---

## 2. Endpoints & Browser Token OAuth Flow
- **Chat Completions Endpoint**: `POST https://llm.kimchi.dev/openai/v1/chat/completions`
- **Models Metadata Endpoint**: `GET https://llm.kimchi.dev/v1/models/metadata?include_in_cli=true`
- **Browser Token Auth Flow (`src/lib/oauth/providers/kimchi.js`)**:
  - **Auth URL**: `https://app.kimchi.dev/cli-auth?callback=<redirectUri>&state=<state>`
  - **Token Validation**: `GET https://api.cast.ai/v1/llm/openai/supported-providers` with `Authorization: Bearer <token>`
  - **User Info**: `GET https://app.kimchi.dev/api/v1/me` with `Authorization: Bearer <token>`

---

## 3. Headers & Request Sanitization (`KimchiExecutor`)
- **Headers**:
  - `Authorization`: `Bearer <accessToken_or_apiKey>`
  - `User-Agent`: `"kimchi/0.1.50"`
  - `Content-Type`: `"application/json"`
- **Request Transformations (`KimchiExecutor.transformRequest`)**:
  1. **System Prompt Merging (`mergeTopLevelSystem`)**: Merges top-level `body.system` into `body.messages` (prepending to existing `role: "system"` message or unshifting a new one) and deletes `body.system`.
  2. **Top-Level Field Stripping**: Deletes `anthropic_version`, `anthropic_beta`, `client_metadata`, `mcp_servers`, `stop_sequences`, `thinking`, `top_k`.
  3. **Anthropic-Backed Model Sanitization (`isAnthropicBackedKimchiModel`)**: When routing to an Anthropic/Claude model on Kimchi, deletes `reasoning_effort`, `reasoning`, and `thinking`.
  4. **Artifact Stripping (`stripMessageArtifacts`, `stripToolArtifacts`, `stripReasoningContent`)**:
     - Strips `cache_control` and `signature` from messages, content parts, and tools.
     - Strips `reasoning_content` from assistant messages when its length exceeds `REASONING_PLACEHOLDER_MAX_LEN` (`8` chars), keeping short 1-char placeholders intact.

---

## 4. Supported Models
| Model ID | Display Name |
|---|---|
| `minimax-m3` | MiniMax-M3 |
| `kimi-k2.7` | Kimi-K2.7 |
| `kimi-k2.6` | Kimi-K2.6 |
| `kimi-k2.5` | Kimi-K2.5 |
| `nemotron-3-ultra-fp4` | Nemotron 3 Ultra FP4 |
| `minimax-m2.7` | MiniMax-M2.7 |
| `claude-opus-4-6` | Claude Opus 4.6 |
| `claude-sonnet-4-6` | Claude Sonnet 4.6 |

---

## 5. Verbatim Source Code Snippets

### `open-sse/executors/kimchi.js` (Excerpt)
```javascript
const TOP_LEVEL_OPENAI_GATEWAY_DROPS = [
  "anthropic_version",
  "anthropic_beta",
  "client_metadata",
  "mcp_servers",
  "stop_sequences",
  "thinking",
  "top_k",
];

export class KimchiExecutor extends DefaultExecutor {
  constructor() {
    super("kimchi");
  }

  transformRequest(model, body, stream, credentials) {
    const transformed = super.transformRequest(model, body, stream, credentials);
    if (!transformed || typeof transformed !== "object") return transformed;

    mergeTopLevelSystem(transformed);
    for (const key of TOP_LEVEL_OPENAI_GATEWAY_DROPS) {
      if (transformed[key] !== undefined) delete transformed[key];
    }
    delete transformed.system;

    if (isAnthropicBackedKimchiModel(model)) {
      delete transformed.reasoning_effort;
      delete transformed.reasoning;
      delete transformed.thinking;
    }

    stripMessageArtifacts(transformed);
    stripToolArtifacts(transformed);
    stripReasoningContent(transformed);
    return transformed;
  }
}
```
