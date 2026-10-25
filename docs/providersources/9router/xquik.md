# Xquik (`xquik`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `xquik`
- **Display Name**: `Xquik`
- **Alias**: `xquik`
- **Category**: `apikey`
- **Service Kinds**: `["webSearch"]`
- **Website**: `https://docs.xquik.com/api-reference/x/search-tweets`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Search Endpoint** | `GET https://xquik.com/api/v1/x/tweets/search?q={query}&limit={maxResults}` |
| **Validation / Credits URL** | `GET https://xquik.com/api/v1/credits` |
| **Auth Header** | `x-api-key: <token>` |
| **Optional Query Parameters** | `cursor`, `queryType` (`"Latest"` or `"Top"`), `language` |

---

## 3. Search Request Builder & Normalizer (`open-sse/handlers/search/callers.js` & `normalizers.js`)

```javascript
function buildXquikRequest(config, params) {
  const apiKey = params.token?.trim();
  if (!apiKey) throw new Error("Xquik search requires an API key");
  const queryType = getProviderSetting(params, "queryType");
  if (queryType && !["Latest", "Top"].includes(queryType)) {
    throw new Error("Xquik queryType must be Latest or Top");
  }

  const qp = new URLSearchParams({
    q: params.query,
    limit: String(params.maxResults),
  });
  const cursor = getProviderSetting(params, "cursor");
  if (cursor) qp.set("cursor", cursor);
  if (queryType) qp.set("queryType", queryType);
  if (params.language) qp.set("language", params.language);

  return {
    url: `${resolveBaseUrl(config, params)}?${qp}`,
    init: {
      method: "GET",
      headers: { Accept: "application/json", "x-api-key": apiKey },
    },
  };
}
```
