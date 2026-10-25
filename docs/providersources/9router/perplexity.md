# Perplexity (`perplexity`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `perplexity`
- **Priority**: `180`
- **Primary Alias**: `perplexity`
- **Additional Aliases**: `["pplx"]`
- **UI Alias**: `pplx`
- **Display Name**: Perplexity
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["llm", "webSearch"]`
- **Website**: `https://www.perplexity.ai`
- **API Key URL**: `https://www.perplexity.ai/settings/api`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source Files**:
  - `open-sse/providers/registry/perplexity.js`
  - `open-sse/handlers/search/callers.js` (`buildPerplexityRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizePerplexity`)

---

## 2. Endpoints by Service Kind
1. **LLM Chat Completions**: `POST https://api.perplexity.ai/chat/completions`
2. **Validation / Models Endpoint**: `GET https://api.perplexity.ai/models`
3. **Web Search via Chat (`searchViaChat`)**: `POST https://api.perplexity.ai/chat/completions` (`defaultModel: "sonar"`)
4. **Native Search API Builder (`buildPerplexityRequest` in `callers.js`)**:
   - Sends `POST` with `Authorization: Bearer <token>` and body `{ query, max_results, country?, search_language_filter?, search_domain_filter? }`.

---

## 3. Supported Models
| Model ID | Display Name |
|---|---|
| `sonar-pro` | Sonar Pro |
| `sonar` | Sonar |

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/perplexity.js`
```javascript
export default {
  id: "perplexity",
  priority: 180,
  alias: "perplexity",
  aliases: [
    "pplx",
  ],
  uiAlias: "pplx",
  display: {
    name: "Perplexity",
    icon: "search",
    color: "#20808D",
    textIcon: "PP",
    website: "https://www.perplexity.ai",
    notice: {
      apiKeyUrl: "https://www.perplexity.ai/settings/api",
    },
  },
  category: "apikey",
  authType: "apikey",
  transport: {
    baseUrl: "https://api.perplexity.ai/chat/completions",
    validateUrl: "https://api.perplexity.ai/models",
  },
  models: [
    { id: "sonar-pro", name: "Sonar Pro" },
    { id: "sonar", name: "Sonar" },
  ],
  serviceKinds: ["llm","webSearch"],
  searchViaChat: {
    defaultModel: "sonar",
    endpoint: "https://api.perplexity.ai/chat/completions",
    pricingUrl: "https://docs.perplexity.ai/guides/pricing",
  },
};
```

### `open-sse/handlers/search/callers.js` (`buildPerplexityRequest`)
```javascript
function buildPerplexityRequest(config, params) {
  const body = { query: params.query, max_results: params.maxResults };
  if (params.country) body.country = params.country;
  if (params.language) body.search_language_filter = [params.language];
  if (params.domainFilter?.length) body.search_domain_filter = params.domainFilter;
  return {
    url: resolveBaseUrl(config, params),
    init: {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${params.token}` },
      body: JSON.stringify(body),
    },
  };
}
```
