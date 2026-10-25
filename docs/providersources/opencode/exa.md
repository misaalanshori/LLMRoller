# Exa Web Search (`exa`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.websearch.exa` (`packages/core/src/plugin/websearch/exa.ts`)
- **Integration ID**: `exa` (`Exa`)
- **Service Kind**: Web Search (via Model Context Protocol JSON-RPC over HTTP)

---

## 2. Endpoint & Authentication

| Parameter | Value |
|---|---|
| **MCP Endpoint** | `POST https://mcp.exa.ai/mcp` |
| **Authentication** | Query parameter `?exaApiKey=<key>` (env fallback `EXA_API_KEY`) |
| **MCP Tool Name** | `web_search_exa` |
| **MCP Arguments** | `{ query: input.query, numResults: 8 }` |

---

## 3. Implementation (`packages/core/src/plugin/websearch/exa.ts`)

```typescript
export const endpoint = "https://mcp.exa.ai/mcp"

const McpInput = Schema.Struct({
  query: Schema.String,
  numResults: Schema.Number.pipe(Schema.optional),
})

export const Plugin = define<HttpClient.HttpClient | Scope.Scope>({
  id: "opencode.websearch.exa",
  effect: Effect.fn("WebSearchExa.Plugin")(function* (ctx) {
    const http = yield* HttpClient.HttpClient
    yield* ctx.websearch.transform((editor) => {
      editor.add({
        id: "exa",
        name: "Exa",
        execute: (input) =>
          Effect.gen(function* () {
            const connection = yield* ctx.integration.connection.active("exa")
            const credential = connection ? yield* ctx.integration.connection.resolve(connection) : undefined
            const url = new URL(endpoint)
            if (credential?.type === "key") url.searchParams.set("exaApiKey", credential.key)
            const result = yield* WebSearchMcp.call(
              http,
              url.toString(),
              "web_search_exa",
              { input: McpInput, output: McpOutput },
              { query: input.query, numResults: 8 },
            )
            const content = result?.content.find((item) => item.text)
            return content ? parseResults(content.text) : []
          }),
      })
    })
  }),
})
```
