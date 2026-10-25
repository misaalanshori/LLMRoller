# Firecrawl Web Search (`firecrawl`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.websearch.firecrawl` (`packages/core/src/plugin/websearch/firecrawl.ts`)
- **Integration ID**: `firecrawl` (`Firecrawl`)
- **Service Kind**: Web Search (via Model Context Protocol JSON-RPC over HTTP)

---

## 2. Endpoint & Authentication

| Parameter | Value |
|---|---|
| **MCP Endpoint** | `POST https://mcp.firecrawl.dev/v2/mcp` |
| **Authentication** | `Authorization: Bearer <key>` (env fallback `FIRECRAWL_API_KEY`) |
| **MCP Tool Name** | `firecrawl_search` |
| **MCP Arguments** | `{ query: input.query, limit: 8 }` |

---

## 3. Implementation (`packages/core/src/plugin/websearch/firecrawl.ts`)

```typescript
export const endpoint = "https://mcp.firecrawl.dev/v2/mcp"

const McpInput = Schema.Struct({
  query: Schema.String,
  limit: Schema.Number.pipe(Schema.optional),
})

export const Plugin = define<HttpClient.HttpClient | Scope.Scope>({
  id: "opencode.websearch.firecrawl",
  effect: Effect.fn("WebSearchFirecrawl.Plugin")(function* (ctx) {
    const http = yield* HttpClient.HttpClient
    yield* ctx.websearch.transform((editor) => {
      editor.add({
        id: "firecrawl",
        name: "Firecrawl",
        execute: (input) =>
          Effect.gen(function* () {
            const connection = yield* ctx.integration.connection.active("firecrawl")
            const credential = connection ? yield* ctx.integration.connection.resolve(connection) : undefined
            const result = yield* WebSearchMcp.call(
              http,
              endpoint,
              "firecrawl_search",
              { input: McpInput, output: McpOutput },
              { query: input.query, limit: 8 },
              {
                "User-Agent": App.useragent(ctx.app),
                ...(credential?.type === "key" ? { Authorization: `Bearer ${credential.key}` } : {}),
              },
            )
            const content = result?.content.find((item) => item.text)
            const response = content ? Option.getOrUndefined(decodeSearchResponse(content.text)) : undefined
            return (
              response?.data.web.map((item) => ({
                url: item.url,
                ...(item.title ? { title: item.title } : {}),
                ...(item.description ? { content: item.description } : {}),
                time: {},
              })) ?? []
            )
          }),
      })
    })
  }),
})
```
