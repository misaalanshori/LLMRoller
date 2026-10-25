# Ollama Search (`ollama-search`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `ollama-search`
- **Alias**: `ollama-search`
- **Display Name**: Ollama Search
- **Category**: `apikey`
- **Auth Type**: `apikey` (`authModes: ["apikey"]`, `credentialFallback: "ollama"` — reuses the API key stored under the `ollama` chat provider)
- **Service Kinds**: `["webSearch"]`
- **Website**: `https://ollama.com`
- **API Key URL**: `https://ollama.com/settings/keys`
- **Source Files**:
  - `open-sse/providers/registry/ollama-search.js`
  - `open-sse/handlers/search/callers.js` (`buildOllamaSearchRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizeOllamaSearch`)

---

## 2. Endpoints & Configuration
- **Web Search Endpoint**: `POST https://ollama.com/api/web_search`
- **Quotas & Limits**:
  - `costPerQuery`: `0`
  - `freeMonthlyQuota`: `1000`
  - `searchTypes`: `["web"]`
  - `defaultMaxResults`: `5`
  - `maxMaxResults`: `10`
  - `timeoutMs`: `10000`
  - `cacheTTLMs`: `300000`

---

## 3. Request & Response Format
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <token>`
- **Request Body (`buildOllamaSearchRequest`)**:
  ```json
  {
    "query": "<query>",
    "max_results": 5,
    "country": "<optional_country>",
    "language": "<optional_language>"
  }
  ```
- **Response Normalization (`normalizeOllamaSearch`)**:
  - Maps `data.results[]` into `SearchResult` (`title: item.title`, `url: item.url`, `snippet: item.content || item.snippet || ""`, `full_text: item.content`, `published_at: item.published_at || null`, `source_type: item.source || null`).

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/ollama-search.js`
```javascript
export default {
  id: "ollama-search",
  alias: "ollama-search",
  display: {
    name: "Ollama Search",
    icon: "cloud",
    color: "#ffffff",
    textIcon: "OL",
    website: "https://ollama.com",
    notice: {
      text: "Web search via Ollama Cloud subscription. Reuses the API key from the Ollama (chat) provider.",
      apiKeyUrl: "https://ollama.com/settings/keys",
    },
  },
  category: "apikey",
  authType: "apikey",
  authModes: ["apikey"],
  serviceKinds: ["webSearch"],
  credentialFallback: "ollama",
  searchConfig: {
    baseUrl: "https://ollama.com/api/web_search",
    method: "POST",
    authType: "apikey",
    authHeader: "bearer",
    costPerQuery: 0,
    freeMonthlyQuota: 1000,
    searchTypes: ["web"],
    defaultMaxResults: 5,
    maxMaxResults: 10,
    timeoutMs: 10000,
    cacheTTLMs: 300000,
  },
};
```

### `open-sse/handlers/search/callers.js` (`buildOllamaSearchRequest`)
```javascript
function buildOllamaSearchRequest(config, params) {
  const body = { query: params.query, max_results: params.maxResults };
  if (params.country) body.country = params.country;
  if (params.language) body.language = params.language;
  return {
    url: resolveBaseUrl(config, params),
    init: {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(params.token ? { Authorization: `Bearer ${params.token}` } : {}),
      },
      body: JSON.stringify(body),
    },
  };
}
```
