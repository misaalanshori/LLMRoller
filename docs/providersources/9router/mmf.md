# MMF (`mmf`) — Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `mmf`
- **Priority**: `200`
- **Hidden**: `true`
- **Display Name**: MMF
- **Category**: `apikey` (`transport.noAuth: true`)
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Source File**: `open-sse/providers/registry/mmf.js`

---

## 2. Endpoints & Transport
- **Chat Endpoint**: `POST https://api.xiaomimimo.com/api/free-ai/openai/chat`
- **No Auth**: `noAuth: true`
- **Supported Models**:
  - `mimo-auto` ("MiMo Auto")

---

## 3. Verbatim Source Code

### `open-sse/providers/registry/mmf.js`
```javascript
export default {
  id: "mmf",
  hidden: true,
  priority: 200,
  display: {
    name: "MMF",
    icon: "hub",
    color: "#6366F1",
    textIcon: "MF",
  },
  category: "apikey",
  transport: {
    baseUrl: "https://api.xiaomimimo.com/api/free-ai/openai/chat",
    noAuth: true,
  },
  models: [
    { id: "mimo-auto", name: "MiMo Auto" },
  ],
};
```
