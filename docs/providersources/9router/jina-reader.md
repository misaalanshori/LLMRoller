# Jina Reader (`jina-reader`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `jina-reader`
- **Alias**: `jina-reader`
- **Display Name**: Jina Reader
- **Category**: `apikey`
- **Auth Type**: `apikey`
- **Service Kinds**: `["webFetch"]`
- **Website**: `https://jina.ai/reader`
- **API Key URL**: `https://jina.ai/?sui=apikey`
- **Source Files**:
  - `open-sse/providers/registry/jina-reader.js`
  - `open-sse/handlers/fetch/index.js` (`runJina`, `parseJinaTitle`)

---

## 2. Endpoints & Configuration
- **Registry Base URL**: `https://r.jina.ai`
- **Actual Request in `runJina`**: `POST https://r.jina.ai/`
- **Quotas & Limits**:
  - `costPerQuery`: `0`
  - `freeMonthlyQuota`: `1000000`
  - `formats`: `["markdown", "text", "html"]`
  - `maxCharacters`: `200000`
  - `timeoutMs`: `30000` (30s)

---

## 3. Request & Response Format (`runJina`)
- **Headers**:
  - `content-type: application/json`
  - `authorization: Bearer <apiKey>` (optional if `apiKey` is empty)
- **Request Body**: `{ "url": "<url>" }`
- **Response Parsing**:
  - Reads plain text response body (`r.res.text()`), truncates to `maxCharacters`, and extracts the title via `parseJinaTitle(body)` (matches `/^\s*Title:\s*(.+)$/mi` or `/^\s*#\s+(.+)$/m`).

---

## 4. Verbatim Source Code Snippets

### `open-sse/providers/registry/jina-reader.js`
```javascript
export default {
  id: "jina-reader",
  alias: "jina-reader",
  display: {
    name: "Jina Reader",
    icon: "menu_book",
    color: "#000000",
    textIcon: "JR",
    website: "https://jina.ai/reader",
    notice: {
      apiKeyUrl: "https://jina.ai/?sui=apikey"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "webFetch"
  ],
  fetchConfig: {
    baseUrl: "https://r.jina.ai",
    method: "GET",
    authType: "apikey",
    authHeader: "bearer",
    costPerQuery: 0,
    freeMonthlyQuota: 1000000,
    formats: [
      "markdown",
      "text",
      "html"
    ],
    maxCharacters: 200000,
    timeoutMs: 30000
  }
};
```

### `open-sse/handlers/fetch/index.js` (`runJina` & `parseJinaTitle`)
```javascript
function parseJinaTitle(text) {
  const source = String(text || "");
  const metadataTitle = source.match(/^\s*Title:\s*(.+)$/mi);
  if (metadataTitle) return metadataTitle[1].trim();
  const m = source.match(/^\s*#\s+(.+)$/m);
  return m ? m[1].trim() : null;
}

async function runJina({ url, fmt, timeoutMs, apiKey, maxCharacters, costPerQuery, startedAt }) {
  const upstreamStart = Date.now();
  const r = await tryFetch("https://r.jina.ai/", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      ...(apiKey ? { authorization: `Bearer ${apiKey}` } : {})
    },
    body: JSON.stringify({ url })
  }, timeoutMs);

  if (!r.ok) {
    return { success: false, status: r.timeout ? 504 : 502, error: r.error };
  }
  const upstreamMs = Date.now() - upstreamStart;
  const body = await r.res.text();
  if (!r.res.ok) {
    return { success: false, status: r.res.status, error: body?.slice(0, 500) || `Jina error: ${r.res.status}` };
  }
  const text = truncate(body, maxCharacters);
  return {
    success: true,
    data: buildData({
      provider: "jina-reader", url, title: parseJinaTitle(body), format: fmt, text,
      costUsd: costPerQuery, responseMs: Date.now() - startedAt, upstreamMs
    })
  };
}
```
