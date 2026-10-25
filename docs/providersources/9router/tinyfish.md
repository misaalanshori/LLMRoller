# TinyFish (`tinyfish`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `tinyfish`
- **Display Name**: `TinyFish`
- **Category**: `apikey`
- **Service Kinds**: `["search", "fetch"]`
- **Website**: `https://tinyfish.ai`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Search Endpoint** | `GET https://api.search.tinyfish.ai?query={query}` |
| **Fetch Endpoint** | `POST https://api.fetch.tinyfish.ai` |
| **Auth Header** | `X-API-Key: <token>` |

---

## 3. Search & Fetch Implementation (`src/lib/search/providers/registry.js`)

```javascript
export function buildTinyFishRequest(query, token, { maxResults = 5, searchType = "web", country, language, includeDomains, excludeDomains, page } = {}) {
  const params = new URLSearchParams({ query });
  if (searchType === "news") params.set("domain_type", "news");
  else if (searchType === "research") params.set("domain_type", "research_paper");
  if (country) params.set("location", country);
  if (language) params.set("language", language.toLowerCase());
  if (includeDomains?.length) params.set("include_domains", includeDomains.slice(0, 50).join(","));
  if (excludeDomains?.length) params.set("exclude_domains", excludeDomains.slice(0, 50).join(","));
  if (page && page > 1) params.set("page", String(page));
  return {
    url: `https://api.search.tinyfish.ai?${params.toString()}`,
    options: { method: "GET", headers: { "X-API-Key": token } },
  };
}

export function normalizeTinyFish(data, maxResults = 5) {
  const raw = data?.results || [];
  return {
    results: raw.slice(0, maxResults).map((r) => ({
      title: r.title || "",
      url: r.url || "",
      snippet: r.snippet || "",
    })),
    total: raw.length,
  };
}

export function buildTinyFishFetchRequest(url, token, { format = "markdown" } = {}) {
  return {
    url: "https://api.fetch.tinyfish.ai",
    options: {
      method: "POST",
      headers: { "X-API-Key": token, "Content-Type": "application/json" },
      body: JSON.stringify({ urls: [url], format: format === "html" ? "html" : "markdown" }),
    },
  };
}

export function normalizeTinyFishFetch(data, url) {
  const item = (data?.results || [])[0] || {};
  return {
    url: item.url || url,
    title: item.title || "",
    content: item.text || "",
  };
}
```
