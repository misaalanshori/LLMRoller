# Serper (`serper`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `serper`
- **Display Name**: `Serper`
- **Category**: `apikey`
- **Service Kinds**: `["search"]`
- **Website**: `https://serper.dev`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Web Search Endpoint** | `POST https://google.serper.dev/search` |
| **News Search Endpoint** | `POST https://google.serper.dev/news` |
| **Auth Header** | `X-API-Key: <token>` |

---

## 3. Search Request Builder & Response Normalizer (`src/lib/search/providers/registry.js`)

```javascript
export function buildSerperRequest(query, token, { maxResults = 5, searchType = "web", country, language } = {}) {
  const endpoint = searchType === "news" ? "https://google.serper.dev/news" : "https://google.serper.dev/search";
  const body = {
    q: query,
    num: maxResults,
    ...(country ? { gl: country.toLowerCase() } : {}),
    ...(language ? { hl: language.toLowerCase() } : {}),
  };
  return {
    url: endpoint,
    options: {
      method: "POST",
      headers: { "X-API-Key": token, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    },
  };
}

export function normalizeSerper(data, maxResults = 5) {
  const raw = data?.organic || data?.news || [];
  return {
    results: raw.slice(0, maxResults).map((r) => ({
      title: r.title || "",
      url: r.link || "",
      snippet: r.snippet || "",
      ...(r.date ? { published_at: r.date } : {}),
    })),
    total: raw.length,
  };
}
```
