# Tavily Web Search (`tavily`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.websearch.tavily` (`packages/core/src/plugin/websearch/tavily.ts`)
- **Integration ID**: `tavily` (`Tavily`)
- **Service Kind**: Web Search (REST HTTP JSON)

---

## 2. Endpoint, Headers & Keyless Mode

| Parameter | Value |
|---|---|
| **Search Endpoint** | `POST https://api.tavily.com/search` |
| **Client Header** | `X-Client-Name: opencode2` |
| **Keyed Auth** | `Authorization: Bearer <key>` (env fallback `TAVILY_API_KEY`) |
| **Keyless Mode (No API Key)** | `X-Tavily-Access-Mode: keyless` |
| **Request Body** | `{ query: input.query, search_depth: "basic", chunks_per_source: 3, max_results: 8 }` |

---

## 3. Implementation (`packages/core/src/plugin/websearch/tavily.ts`)

```typescript
export const endpoint = "https://api.tavily.com/search"

const request = yield* HttpClientRequest.post(endpoint).pipe(
  HttpClientRequest.acceptJson,
  HttpClientRequest.setHeaders({
    "User-Agent": App.useragent(ctx.app),
    "X-Client-Name": "opencode2",
    ...(credential?.type === "key"
      ? { Authorization: `Bearer ${credential.key}` }
      : { "X-Tavily-Access-Mode": "keyless" }),
  }),
  HttpClientRequest.schemaBodyJson(SearchRequest)({
    query: input.query,
    search_depth: "basic",
    chunks_per_source: 3,
    max_results: 8,
  }),
)
```
