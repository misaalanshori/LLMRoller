# Parallel Web Search (`parallel`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.websearch.parallel` (`packages/core/src/plugin/websearch/parallel.ts`)
- **Integration ID**: `parallel` (`Parallel`)
- **Service Kind**: Web Search (via Model Context Protocol JSON-RPC over HTTP)

---

## 2. Endpoint & Authentication

| Parameter | Value |
|---|---|
| **MCP Endpoint** | `POST https://search.parallel.ai/mcp` |
| **Authentication** | `Authorization: Bearer <key>` (env fallback `PARALLEL_API_KEY`) |
| **MCP Tool Name** | `web_search` |
| **MCP Arguments** | `{ objective: input.query, search_queries: [input.query] }` |

---

## 3. Implementation (`packages/core/src/plugin/websearch/parallel.ts`)

```typescript
export const endpoint = "https://search.parallel.ai/mcp"

const McpInput = Schema.Struct({
  objective: Schema.String,
  search_queries: Schema.Array(Schema.String),
  model_name: Schema.String.check(Schema.isMaxLength(100)).pipe(Schema.optional),
})

export const Plugin = define<HttpClient.HttpClient | Scope.Scope>({
  id: "opencode.websearch.parallel",
  effect: Effect.fn("WebSearchParallel.Plugin")(function* (ctx) {
    const http = yield* HttpClient.HttpClient
    yield* ctx.websearch.transform((editor) => {
      editor.add({
        id: "parallel",
        name: "Parallel",
        execute: (input) =>
          Effect.gen(function* () {
            const connection = yield* ctx.integration.connection.active("parallel")
            const credential = connection ? yield* ctx.integration.connection.resolve(connection) : undefined
            const result = yield* WebSearchMcp.call(
              http,
              endpoint,
              "web_search",
              { input: McpInput, output: McpOutput },
              {
                objective: input.query,
                search_queries: [input.query],
              },
              {
                "User-Agent": App.useragent(ctx.app),
                ...(credential?.type === "key" ? { Authorization: `Bearer ${credential.key}` } : {}),
              },
            )
            return (
              result?.structuredContent.results.map((item) => {
                const published = item.publish_date ? Date.parse(item.publish_date) : undefined
                return {
                  url: item.url,
                  ...(item.title ? { title: item.title } : {}),
                  ...(item.excerpts.length ? { content: item.excerpts.join("\n\n") } : {}),
                  time: { ...(published !== undefined && Number.isFinite(published) ? { published } : {}) },
                }
              }) ?? []
            )
          }),
      })
    })
  }),
})
```
