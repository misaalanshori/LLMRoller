# SearchApi (`searchapi`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `searchapi`
- **Display Name**: `SearchApi`
- **Category**: `apikey` (`authType: "apikey"`, `authHeader: "query_param"`)
- **Service Kinds**: `["search"]`
- **Website**: `https://www.searchapi.io`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Search Endpoint** | `GET https://www.searchapi.io/api/v1/search?engine={google\|google_news}&q={query}&api_key={token}` |
| **Auth Method** | Query parameter `?api_key=<token>` (`authHeader: "query_param"`) |

---

## 3. Search Request Builder & Response Normalizer (`src/lib/search/providers/registry.js`)

```javascript
export function buildSearchApiRequest(query, token, { maxResults = 5, searchType = "web", country, language, page } = {}) {
  const engine = searchType === "news" ? "google_news" : "google";
  const params = new URLSearchParams({ engine, q: query, api_key: token });
  if (country) params.set("gl", country.toLowerCase());
  if (language) params.set("hl", language.toLowerCase());
  if (page && page > 1) params.set("page", String(page));
  return { url: `https://www.searchapi.io/api/v1/search?${params.toString()}`, options: { method: "GET" } };
}

export function normalizeSearchApi(data, maxResults = 5) {
  const raw = data?.organic_results || data?.news_results || [];
  return {
    results: raw.slice(0, maxResults).map((r) => ({
      title: r.title || "",
      url: r.link || "",
      snippet: r.snippet || "",
      ...(r.date ? { published_at: r.date } : {}),
    })),
    total: data?.search_information?.total_results ?? raw.length,
  };
}
```
