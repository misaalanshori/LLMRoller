# SearXNG (`searxng`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `searxng`
- **Display Name**: `SearXNG`
- **Alias**: `sxng`
- **Category**: `apikey` (`noAuth: true`)
- **Service Kinds**: `["search"]`
- **Website**: `https://docs.searxng.org`

---

## 2. Endpoints & Configuration

| Parameter | Value |
|---|---|
| **Search Endpoint** | `GET ${SEARXNG_URL}/search` (Default `http://localhost:8888/search`) |
| **Auth Method** | None (`noAuth: true`) |
| **Query Parameters** | `q`, `format=json`, `categories=general\|news`, `language`, `time_range`, `pageno` |

---

## 3. Search Request Builder & Response Normalizer (`src/lib/search/providers/registry.js`)

```javascript
export function buildSearxngRequest(query, _token, { maxResults = 5, searchType = "web", language, timeRange, page } = {}) {
  const base = (process.env.SEARXNG_URL || "http://localhost:8888").replace(/\/$/, "");
  const params = new URLSearchParams({
    q: query,
    format: "json",
    categories: searchType === "news" ? "news" : "general",
  });
  if (language) params.set("language", language.toLowerCase());
  if (timeRange && timeRange !== "any") params.set("time_range", timeRange);
  if (page && page > 1) params.set("pageno", String(page));
  return { url: `${base}/search?${params.toString()}`, options: { method: "GET" } };
}

export function normalizeSearxng(data, maxResults = 5) {
  const raw = data?.results || [];
  return {
    results: raw.slice(0, maxResults).map((r) => ({
      title: r.title || "",
      url: r.url || "",
      snippet: r.content || "",
      ...(r.publishedDate ? { published_at: r.publishedDate } : {}),
    })),
    total: data?.number_of_results ?? raw.length,
  };
}
```
