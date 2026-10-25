# Firecrawl (`firecrawl`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `firecrawl`
- **Alias**: `firecrawl`
- **Display Name**: Firecrawl
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["webFetch"]`
- **Website**: `https://firecrawl.dev`
- **API Key URL**: `https://www.firecrawl.dev/app/api-keys`
- **Source Files**:
  - `open-sse/providers/registry/firecrawl.js`
  - `open-sse/handlers/fetch/index.js` (`runFirecrawl`)

---

## 2. Endpoints & Configuration
- **Scrape Endpoint**: `POST https://api.firecrawl.dev/v1/scrape`
- **Quotas & Limits**:
  - `costPerQuery`: `$0.002`
  - `freeMonthlyQuota`: `500`
  - `formats`: `["markdown", "html", "text"]`
  - `maxCharacters`: `200000`
  - `timeoutMs`: `30000` (30s)

---

## 3. Authentication, Request & Response Format
- **Headers**:
  - `content-type: application/json`
  - `authorization: Bearer <apiKey>`
- **Request Body (`runFirecrawl`)**:
  ```json
  {
    "url": "<url>",
    "formats": ["markdown"]
  }
  ```
- **Response Normalization**:
  - Reads `json.data`: extracts `d.markdown || d.html || d.text || ""` (truncated to `maxCharacters`) and `d.metadata?.title || null`.

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/firecrawl.js`
```javascript
export default {
  id: "firecrawl",
  alias: "firecrawl",
  display: {
    name: "Firecrawl",
    icon: "local_fire_department",
    color: "#F59E0B",
    textIcon: "FC",
    website: "https://firecrawl.dev",
    notice: {
      apiKeyUrl: "https://www.firecrawl.dev/app/api-keys"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "webFetch"
  ],
  fetchConfig: {
    baseUrl: "https://api.firecrawl.dev/v1/scrape",
    method: "POST",
    authType: "apikey",
    authHeader: "bearer",
    costPerQuery: 0.002,
    freeMonthlyQuota: 500,
    formats: [
      "markdown",
      "html",
      "text"
    ],
    maxCharacters: 200000,
    timeoutMs: 30000
  }
};
```

### `open-sse/handlers/fetch/index.js` (`runFirecrawl`)
```javascript
async function runFirecrawl({ url, fmt, timeoutMs, apiKey, maxCharacters, costPerQuery, startedAt }) {
  const upstreamStart = Date.now();
  const r = await tryFetch("https://api.firecrawl.dev/v1/scrape", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {})
    },
    body: JSON.stringify({ url, formats: [fmt] })
  }, timeoutMs);

  if (!r.ok) {
    return { success: false, status: r.timeout ? 504 : 502, error: r.error };
  }
  const upstreamMs = Date.now() - upstreamStart;
  const { json } = await readJsonOrText(r.res);
  if (!r.res.ok) {
    return { success: false, status: r.res.status, error: json?.error || `Firecrawl error: ${r.res.status}` };
  }
  const d = json?.data || {};
  const text = truncate(d.markdown || d.html || d.text || "", maxCharacters);
  const title = d.metadata?.title || null;
  return {
    success: true,
    data: buildData({
      provider: "firecrawl", url, title, format: fmt, text,
      costUsd: costPerQuery, responseMs: Date.now() - startedAt, upstreamMs
    })
  };
}
```
