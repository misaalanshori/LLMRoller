# Brave Search (`brave-search`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `brave-search`
- **Alias**: `brave`
- **Display Name**: Brave Search
- **Category**: `apikey`
- **Service Kinds**: `["webSearch"]`
- **Website**: `https://brave.com/search/api`
- **API Key URL**: `https://api-dashboard.search.brave.com/app/keys`
- **Source Files**:
  - `open-sse/providers/registry/brave-search.js`
  - `open-sse/handlers/search/callers.js` (`buildBraveRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizeBrave`)

---

## 2. Endpoints, Methods & Configuration
- **Base URL**: `https://api.search.brave.com/res/v1` (overridable via `providerOptions.baseUrl` or `providerSpecificData.baseUrl` after SSRF validation via `assertPublicUrl`)
- **Web Search Endpoint**: `GET https://api.search.brave.com/res/v1/web/search?q=<query>&count=<maxResults>[&country=<country>][&search_lang=<language>]`
- **News Search Endpoint**: `GET https://api.search.brave.com/res/v1/news/search?q=<query>&count=<maxResults>[&country=<country>][&search_lang=<language>]`
- **Search Parameters & Quotas**:
  - `searchTypes`: `["web", "news"]`
  - `defaultMaxResults`: `5`
  - `maxMaxResults`: `20`
  - `costPerQuery`: `$0.005`
  - `freeMonthlyQuota`: `1000`
  - `timeoutMs`: `10000` (10s)
  - `cacheTTLMs`: `300000` (5 minutes)

---

## 3. Authentication & Headers
- **Auth Header**: `X-Subscription-Token: <token>` (configured as `authHeader: "x-subscription-token"` in registry, sent as `"X-Subscription-Token"` in `buildBraveRequest`)
- **Accept Header**: `Accept: application/json`

---

## 4. Request Construction & Response Normalization
- **Request Query Parameters**:
  - `q`: Search query string (`params.query`)
  - `count`: Stringified `params.maxResults`
  - `country`: Optional country code (`params.country`)
  - `search_lang`: Optional language code (`params.language`)
- **Response Parsing (`normalizeBrave`)**:
  - Selects container `data.news || data` when `searchType === "news"`, otherwise `data.web`.
  - Extracts `container.results` array and maps each item to `SearchResult`:
    - `title`: `item.title`
    - `url`: `item.url`
    - `snippet`: `item.description`
    - `published_at`: `item.page_age || item.age`
    - `favicon_url`: `item.meta_url?.favicon || item.favicon`
  - `totalResults`: `container?.totalCount ?? null`

---

## 5. Verbatim Source Code Snippets

### `open-sse/providers/registry/brave-search.js`
```javascript
export default {
  id: "brave-search",
  alias: "brave",
  display: {
    name: "Brave Search",
    icon: "travel_explore",
    color: "#FB542B",
    textIcon: "BR",
    website: "https://brave.com/search/api",
    notice: {
      apiKeyUrl: "https://api-dashboard.search.brave.com/app/keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "webSearch"
  ],
  searchConfig: {
    baseUrl: "https://api.search.brave.com/res/v1",
    method: "GET",
    authType: "apikey",
    authHeader: "x-subscription-token",
    costPerQuery: 0.005,
    freeMonthlyQuota: 1000,
    searchTypes: [
      "web",
      "news"
    ],
    defaultMaxResults: 5,
    maxMaxResults: 20,
    timeoutMs: 10000,
    cacheTTLMs: 300000
  }
};
```

### `open-sse/handlers/search/callers.js` (`buildBraveRequest`)
```javascript
function buildBraveRequest(config, params) {
  const endpoint = params.searchType === "news" ? "/news/search" : "/web/search";
  const qp = new URLSearchParams({ q: params.query, count: String(params.maxResults) });
  if (params.country) qp.set("country", params.country);
  if (params.language) qp.set("search_lang", params.language);
  return {
    url: `${resolveBaseUrl(config, params)}${endpoint}?${qp}`,
    init: {
      method: "GET",
      headers: { Accept: "application/json", "X-Subscription-Token": params.token },
    },
  };
}
```

### `open-sse/handlers/search/normalizers.js` (`normalizeBrave`)
```javascript
function normalizeBrave(data, _query, searchType) {
  const now = new Date().toISOString();
  const container = searchType === "news" ? data.news || data : data.web;
  const items = container?.results;
  if (!Array.isArray(items)) return { results: [], totalResults: null };
  const results = items.map((item, idx) =>
    makeResult("brave-search", {
      title: item.title,
      url: item.url,
      snippet: item.description,
      published_at: item.page_age || item.age,
      favicon_url: item.meta_url?.favicon || item.favicon,
    }, idx, now)
  );
  return { results, totalResults: container?.totalCount ?? null };
}
```
