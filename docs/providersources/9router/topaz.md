# Topaz (`topaz`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `topaz`
- **Display Name**: `Topaz`
- **Category**: `apikey`
- **Service Kinds**: `["image"]`
- **Website**: `https://topazlabs.com`

---

## 2. Configuration (`src/shared/constants/providers.js`)

`topaz` is registered in `APIKEY_PROVIDERS` for Topaz Labs API key management:

```javascript
topaz: {
  id: "topaz",
  name: "Topaz",
  category: "apikey",
  color: "#00B4D8",
  textIcon: "TZ",
  serviceKinds: ["image"],
  website: "https://topazlabs.com",
},
```
