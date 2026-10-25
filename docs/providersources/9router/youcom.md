# You.com Search (`youcom`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `youcom`
- **Display Name**: `You.com Search`
- **Alias**: `youcom`
- **Category**: `apikey`
- **Service Kinds**: `["webSearch"]`
- **Website**: `https://you.com`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Search Endpoint** | `GET https://ydc-index.io/v1/search` |
| **Auth Header** | `x-api-key: <token>` (`X-API-Key: <token>`) |
| **Supported Search Types** | `["web", "news"]` |

---

## 3. Registry & Normalizer (`open-sse/providers/registry/youcom.js` & `open-sse/handlers/search/normalizers.js`)

```javascript
export default {
  id: "youcom",
  alias: "youcom",
  category: "apikey",
  authType: "apikey",
  serviceKinds: ["webSearch"],
  searchConfig: {
    baseUrl: "https://ydc-index.io/v1/search",
    method: "GET",
    authType: "apikey",
    authHeader: "x-api-key",
    costPerQuery: 0.005,
    freeMonthlyQuota: 0,
    searchTypes: ["web", "news"],
    defaultMaxResults: 5,
    maxMaxResults: 100,
    timeoutMs: 10000,
    cacheTTLMs: 300000,
  },
};
```
