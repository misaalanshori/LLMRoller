# Google Programmable Search Engine (`google-pse`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `google-pse`
- **Alias**: `gpse`
- **Display Name**: Google PSE
- **Category**: `apikey`
- **Auth Type**: `apikey` (requires both `apiKey` and Custom Search Engine ID `cx`)
- **Service Kinds**: `["webSearch"]`
- **Website**: `https://programmablesearchengine.google.com`
- **API Key URL**: `https://programmablesearchengine.google.com/controlpanel/create`
- **Source Files**:
  - `open-sse/providers/registry/google-pse.js`
  - `open-sse/handlers/search/callers.js` (`buildGooglePseRequest`)
  - `open-sse/handlers/search/normalizers.js` (`normalizeGooglePse`)

---

## 2. Endpoints & Configuration
- **Base URL**: `https://www.googleapis.com/customsearch/v1`
- **HTTP Method**: `GET`
- **Quotas & Limits**:
  - `costPerQuery`: `$0.005`
  - `freeMonthlyQuota`: `3000`
  - `searchTypes`: `["web", "news"]`
  - `defaultMaxResults`: `5`
  - `maxMaxResults`: `10`
  - `timeoutMs`: `10000`
  - `cacheTTLMs`: `300000`

---

## 3. Request Construction & Response Normalization
- **Query Parameters (`buildGooglePseRequest`)**:
  - `key`: `<apiKey>` (`params.token`)
  - `cx`: `<cx>` (resolved via `getProviderSetting(params, "cx")` from `providerOptions.cx` or `providerSpecificData.cx`)
  - `q`: `<params.query>`
  - `num`: `String(Math.min(params.maxResults, 10))`
  - `gl`: Optional lowercase country code (`params.country.toLowerCase()`)
  - `hl`: Optional language code (`params.language`)
  - `dateRestrict`: Mapped from `params.timeRange` (`day -> "d1"`, `week -> "w1"`, `month -> "m1"`, `year -> "y1"`)
  - `start`: `String(Math.min(params.offset + 1, 91))` when `params.offset > 0`
- **Headers**: `Accept: application/json`
- **Response Normalization (`normalizeGooglePse`)**:
  - Iterates `data.items[]` and maps each item to `SearchResult` (`title: item.title`, `url: item.link`, `snippet: item.snippet`, `image_url: item.pagemap?.cse_image?.[0]?.src || item.pagemap?.cse_thumbnail?.[0]?.src || item.pagemap?.metatags?.[0]?.["og:image"]`).
  - `totalResults`: `Number(data.searchInformation?.totalResults ?? data.queries?.request?.[0]?.totalResults)`.

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/google-pse.js`
```javascript
export default {
  id: "google-pse",
  alias: "gpse",
  display: {
    name: "Google PSE",
    icon: "search",
    color: "#4285F4",
    textIcon: "GP",
    website: "https://programmablesearchengine.google.com",
    notice: {
      apiKeyUrl: "https://programmablesearchengine.google.com/controlpanel/create"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "webSearch"
  ],
  searchConfig: {
    baseUrl: "https://www.googleapis.com/customsearch/v1",
    method: "GET",
    authType: "apikey",
    authHeader: "key",
    costPerQuery: 0.005,
    freeMonthlyQuota: 3000,
    searchTypes: [
      "web",
      "news"
    ],
    defaultMaxResults: 5,
    maxMaxResults: 10,
    timeoutMs: 10000,
    cacheTTLMs: 300000
  }
};
```

### `open-sse/handlers/search/callers.js` (`buildGooglePseRequest`)
```javascript
function buildGooglePseRequest(config, params) {
  const apiKey = params.token;
  const cx = getProviderSetting(params, "cx");
  if (!apiKey || !cx) {
    throw new Error("Google Programmable Search requires both apiKey and cx");
  }
  const qp = new URLSearchParams({
    key: apiKey,
    cx,
    q: params.query,
    num: String(Math.min(params.maxResults, 10)),
  });
  if (params.country) qp.set("gl", params.country.toLowerCase());
  if (params.language) qp.set("hl", params.language);
  if (params.timeRange && params.timeRange !== "any") {
    const dateRestrictMap = { day: "d1", week: "w1", month: "m1", year: "y1" };
    const dateRestrict = dateRestrictMap[params.timeRange];
    if (dateRestrict) qp.set("dateRestrict", dateRestrict);
  }
  if (typeof params.offset === "number" && params.offset > 0) {
    qp.set("start", String(Math.min(params.offset + 1, 91)));
  }
  return {
    url: `${resolveBaseUrl(config, params)}?${qp}`,
    init: {
      method: "GET",
      headers: { Accept: "application/json" },
    },
  };
}
```
