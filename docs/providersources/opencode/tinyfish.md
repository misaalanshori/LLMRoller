# TinyFish Web Search (`tinyfish`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.websearch.tinyfish` (`packages/core/src/plugin/websearch/tinyfish.ts`)
- **Integration ID**: `tinyfish` (`TinyFish`)
- **Service Kind**: Web Search (via Model Context Protocol JSON-RPC over HTTP)

---

## 2. Endpoint, Session Handshake & Keyless Mode

| Parameter | Value |
|---|---|
| **MCP Endpoint** | `POST https://agent.tinyfish.ai/mcp` |
| **Keyed Auth** | `X-API-Key: <key>` (env fallback `TINYFISH_API_KEY`) + `Mcp-Session-Id: <session>` obtained from `initialize` + `notifications/initialized` |
| **Keyless Mode** | `X-TinyFish-Access-Mode: keyless` (skips `initialize` handshake) |
| **MCP Tool Name** | `search` (`{ query: input.query }`) |

---

## 3. Implementation (`packages/core/src/plugin/websearch/tinyfish.ts`)

```typescript
const headers = {
  "User-Agent": App.useragent(ctx.app),
  ...(credential?.type === "key"
    ? { "X-API-Key": credential.key }
    : { "X-TinyFish-Access-Mode": "keyless" }),
}
// Keyed requests are rejected without the `Mcp-Session-Id` returned by `initialize`; keyless ones are not.
const session = credential?.type === "key" ? yield* initialize(http, headers) : undefined
const result = yield* WebSearchMcp.call(
  http,
  endpoint,
  "search",
  { input: McpInput, output: McpOutput },
  { query: input.query },
  { ...headers, ...(session === undefined ? {} : { "Mcp-Session-Id": session }) },
)
```
