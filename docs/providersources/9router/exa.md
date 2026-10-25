# Exa (`exa`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `exa`
- **Alias**: `exa`
- **Display Name**: Exa
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["webSearch", "webFetch"]`
- **Website**: `https://exa.ai`
- **API Key URL**: `https://dashboard.exa.ai/api-keys`
- **Source Files**:
  - `open-sse/providers/registry/exa.js`
  - `open-sse/handlers/search/callers.js` (`buildExaRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizeExa`)
  - `open-sse/handlers/fetch/index.js` (`runExa`)

---

## 2. Endpoints & Configuration
1. **Web Search (`searchConfig`)**:
   - **Endpoint**: `POST https://api.exa.ai/search`
   - **Headers**: `Content-Type: application/json`, `x-api-key: <token>`
   - **Quotas & Timeouts**: `costPerQuery: $0.007`, `freeMonthlyQuota: 1000`, `searchTypes: ["web", "news"]`, `defaultMaxResults: 5`, `maxMaxResults: 100`, `timeoutMs: 10000`, `cacheTTLMs: 300000`
   - **Request Body (`buildExaRequest`)**:
     ```json
     {
       "query": "<query>",
       "numResults": 5,
       "type": "auto",
       "text": true,
       "highlights": true,
       "includeDomains": ["..."],
       "excludeDomains": ["..."],
       "category": "news"
     }
     ```
     *(Note: `includeDomains`/`excludeDomains` are populated if `domainFilter` is present; `category: "news"` is set when `searchType === "news"`).*
   - **Response Normalization (`normalizeExa`)**:
     - Maps `data.results[]` into unified `SearchResult` with `snippet: item.highlights?.[0] || item.text?.slice(0, 300) || ""`, `full_text: item.text`, `published_at: item.publishedDate`, `favicon_url: item.favicon`, `author: item.author`, `image_url: item.image`, `score: item.score`.
2. **Web Fetch (`fetchConfig`)**:
   - **Endpoint**: `POST https://api.exa.ai/contents`
   - **Headers**: `content-type: application/json`, `x-api-key: <apiKey>`
   - **Quotas & Timeouts**: `costPerQuery: $0.001`, `freeMonthlyQuota: 1000`, `formats: ["text", "markdown"]`, `maxCharacters: 100000`, `timeoutMs: 15000`
   - **Request Body (`runExa`)**:
     ```json
     {
       "ids": ["<url>"],
       "text": true
     }
     ```
   - **Response Normalization**: Extracts `json.results[0].text` and `json.results[0].title`, truncating text to `maxCharacters`.

---

## 3. Verbatim Source Code Snippets

### `open-sse/providers/registry/exa.js`
```javascript
export default {
  id: "exa",
  alias: "exa",
  display: {
    name: "Exa",
    icon: "manage_search",
    color: "#2563EB",
    textIcon: "EX",
    website: "https://exa.ai",
    notice: {
      apiKeyUrl: "https://dashboard.exa.ai/api-keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "webSearch",
    "webFetch"
  ],
  searchConfig: {
    baseUrl: "https://api.exa.ai/search",
    method: "POST",
    authType: "apikey",
    authHeader: "x-api-key",
    costPerQuery: 0.007,
    freeMonthlyQuota: 1000,
    searchTypes: [
      "web",
      "news"
    ],
    defaultMaxResults: 5,
    maxMaxResults: 100,
    timeoutMs: 10000,
    cacheTTLMs: 300000
  },
  fetchConfig: {
    baseUrl: "https://api.exa.ai/contents",
    method: "POST",
    authType: "apikey",
    authHeader: "x-api-key",
    costPerQuery: 0.001,
    freeMonthlyQuota: 1000,
    formats: [
      "text",
      "markdown"
    ],
    maxCharacters: 100000,
    timeoutMs: 15000
  }
};
```

### `open-sse/handlers/search/callers.js` (`buildExaRequest`) & `open-sse/handlers/fetch/index.js` (`runExa`)
```javascript
function buildExaRequest(config, params) {
  const { includes, excludes } = parseDomainFilter(params.domainFilter);
  const body = {
    query: params.query,
    numResults: params.maxResults,
    type: "auto",
    text: true,
    highlights: true,
  };
  if (includes.length) body.includeDomains = includes;
  if (excludes.length) body.excludeDomains = excludes;
  if (params.searchType === "news") body.category = "news";
  return {
    url: resolveBaseUrl(config, params),
    init: {
      method: "POST",
      headers: { "Content-Type": "application/json", "x-api-key": params.token },
      body: JSON.stringify(body),
    },
  };
}

async function runExa({ url, fmt, timeoutMs, apiKey, maxCharacters, costPerQuery, startedAt }) {
  const upstreamStart = Date.now();
  const r = await tryFetch("https://api.exa.ai/contents", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(apiKey ? { "x-api-key": apiKey } : {})
    },
    body: JSON.stringify({ ids: [url], text: true })
  }, timeoutMs);
  if (!r.ok) {
    return { success: false, status: r.timeout ? 504 : 502, error: r.error };
  }
  const upstreamMs = Date.now() - upstreamStart;
  const { json } = await readJsonOrText(r.res);
  if (!r.res.ok) {
    return { success: false, status: r.res.status, error: json?.error || `Exa error: ${r.res.status}` };
  }
  const first = json?.results?.[0] || {};
  const text = truncate(first.text || "", maxCharacters);
  return {
    success: true,
    data: buildData({
      provider: "exa", url, title: first.title || null, format: fmt, text,
      costUsd: costPerQuery, responseMs: Date.now() - startedAt, upstreamMs
    })
  };
}
```
