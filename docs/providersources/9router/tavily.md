# Tavily (`tavily`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `tavily`
- **Display Name**: `Tavily`
- **Category**: `apikey`
- **Service Kinds**: `["search", "fetch"]`
- **Website**: `https://tavily.com`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Search Endpoint** | `POST https://api.tavily.com/search` |
| **Extract / Fetch Endpoint** | `POST https://api.tavily.com/extract` |
| **Authentication** | `Authorization: Bearer <token>` |

---

## 3. Search & Fetch Implementation (`src/lib/search/providers/registry.js`)

```javascript
export function buildTavilyRequest(query, token, { maxResults = 5, searchType = "web", country, includeDomains, excludeDomains } = {}) {
  const body = {
    query,
    max_results: maxResults,
    topic: searchType === "news" ? "news" : "general",
    ...(includeDomains?.length ? { include_domains: includeDomains } : {}),
    ...(excludeDomains?.length ? { exclude_domains: excludeDomains } : {}),
    ...(country ? { country: country.toLowerCase() } : {}),
  };
  return {
    url: "https://api.tavily.com/search",
    options: {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  };
}

export function normalizeTavily(data, maxResults = 5) {
  const raw = data?.results || [];
  return {
    results: raw.slice(0, maxResults).map((r) => ({
      title: r.title || "",
      url: r.url || "",
      snippet: r.content || "",
      ...(r.published_date ? { published_at: r.published_date } : {}),
    })),
    total: raw.length,
  };
}

export function buildTavilyFetchRequest(url, token) {
  return {
    url: "https://api.tavily.com/extract",
    options: {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({ urls: [url], extract_depth: "basic" }),
    },
  };
}

export function normalizeTavilyFetch(data, url) {
  const item = (data?.results || [])[0] || {};
  return {
    url: item.url || url,
    title: item.title || "",
    content: item.raw_content || "",
  };
}
```
