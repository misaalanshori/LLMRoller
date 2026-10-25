# Linkup (`linkup`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `linkup`
- **Alias**: `linkup`
- **Display Name**: Linkup
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["webSearch"]`
- **Website**: `https://linkup.so`
- **API Key URL**: `https://app.linkup.so/api-keys`
- **Source Files**:
  - `open-sse/providers/registry/linkup.js`
  - `open-sse/handlers/search/callers.js` (`buildLinkupRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizeLinkup`)

---

## 2. Endpoints & Configuration
- **Search Endpoint**: `POST https://api.linkup.so/v1/search`
- **Quotas & Limits**:
  - `costPerQuery`: `$0.005`
  - `freeMonthlyQuota`: `1000`
  - `searchTypes`: `["web"]`
  - `defaultMaxResults`: `5`
  - `maxMaxResults`: `50`
  - `timeoutMs`: `10000`
  - `cacheTTLMs`: `300000`

---

## 3. Authentication, Request & Response Format
- **Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <apiKey>`
- **Request Body (`buildLinkupRequest`)**:
  ```json
  {
    "q": "<query>",
    "depth": "standard",
    "outputType": "searchResults",
    "maxResults": 5,
    "includeDomains": ["..."],
    "excludeDomains": ["..."],
    "fromDate": "YYYY-MM-DD",
    "toDate": "YYYY-MM-DD"
  }
  ```
  - `depth`: Configurable via `providerOptions.depth` or `providerSpecificData.depth` (`"fast" | "standard" | "deep"`, defaults to `"standard"`).
  - `fromDate` / `toDate`: Computed from `params.timeRange` (`day`, `week`, `month`, `year`) relative to current UTC date.
- **Response Normalization (`normalizeLinkup`)**:
  - Maps `data.results[]` into `SearchResult` (`title: item.name || item.title`, `url: item.url`, `snippet: item.content || item.snippet || ""`, `full_text: item.content`, `source_type: item.type || "web"`, `image_url: item.image_url || item.imageUrl || null`).

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/linkup.js`
```javascript
export default {
  id: "linkup",
  alias: "linkup",
  display: {
    name: "Linkup",
    icon: "link",
    color: "#0EA5E9",
    textIcon: "LK",
    website: "https://linkup.so",
    notice: {
      apiKeyUrl: "https://app.linkup.so/api-keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "webSearch"
  ],
  searchConfig: {
    baseUrl: "https://api.linkup.so/v1/search",
    method: "POST",
    authType: "apikey",
    authHeader: "bearer",
    costPerQuery: 0.005,
    freeMonthlyQuota: 1000,
    searchTypes: [
      "web"
    ],
    defaultMaxResults: 5,
    maxMaxResults: 50,
    timeoutMs: 10000,
    cacheTTLMs: 300000
  }
};
```

### `open-sse/handlers/search/callers.js` (`buildLinkupRequest`)
```javascript
function buildLinkupRequest(config, params) {
  const apiKey = params.token;
  if (!apiKey) throw new Error("Linkup Search requires an API key");

  const { includes, excludes } = parseDomainFilter(params.domainFilter);
  const requestedDepth = getProviderSetting(params, "depth");
  const depth =
    requestedDepth && ["fast", "standard", "deep"].includes(requestedDepth)
      ? requestedDepth
      : "standard";

  const body = {
    q: params.query,
    depth,
    outputType: "searchResults",
    maxResults: params.maxResults,
  };
  if (includes.length) body.includeDomains = includes;
  if (excludes.length) body.excludeDomains = excludes;
  if (params.timeRange && params.timeRange !== "any") {
    const today = new Date();
    const toDate = today.toISOString().slice(0, 10);
    const from = new Date(today);
    if (params.timeRange === "day") from.setUTCDate(from.getUTCDate() - 1);
    if (params.timeRange === "week") from.setUTCDate(from.getUTCDate() - 7);
    if (params.timeRange === "month") from.setUTCMonth(from.getUTCMonth() - 1);
    if (params.timeRange === "year") from.setUTCFullYear(from.getUTCFullYear() - 1);
    body.fromDate = from.toISOString().slice(0, 10);
    body.toDate = toDate;
  }

  return {
    url: resolveBaseUrl(config, params),
    init: {
      method: "POST",
      headers: { "Content-Type": "application/json", Authorization: `Bearer ${apiKey}` },
      body: JSON.stringify(body),
    },
  };
}
```
