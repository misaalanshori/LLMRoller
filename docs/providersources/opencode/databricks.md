# Databricks (`databricks`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `databricks`
- **Core Plugin**: `packages/core/src/plugin/provider/databricks.ts` (`opencode.provider.databricks`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible` (via `AISDKNative` rewrite)

---

## 2. Authentication & Claude Cache Format Override

- **Environment Variable**: `DATABRICKS_TOKEN` (explicitly filters out `DATABRICKS_HOST` from credential env detection so `DATABRICKS_HOST` only populates the base URL template).
- **Claude Prompt Caching**: Sets `compatibility: { cacheFormat: "anthropic" }` on all Databricks models whose ID includes `"claude"`, since Databricks caches Claude models through explicit `cache_control` breakpoints.

```typescript
import { Effect } from "effect"
import { define } from "@opencode/plugin/effect/plugin"

export const DatabricksPlugin = define({
  id: "opencode.provider.databricks",
  effect: Effect.fn(function* (ctx) {
    yield* ctx.integration.transform((editor) => {
      // models.dev also lists DATABRICKS_HOST, which only fills the base URL template.
      editor.method.update({ integrationID: "databricks", method: { type: "env", names: ["DATABRICKS_TOKEN"] } })
    })
    yield* ctx.model.transform((models) => {
      for (const item of models.list("databricks")) {
        // Databricks caches Claude only through explicit `cache_control` breakpoints.
        if (!(item.modelID ?? item.id).includes("claude")) continue
        models.update(item.providerID, item.id, (model) => {
          model.compatibility = { cacheFormat: "anthropic", ...model.compatibility }
        })
      }
    })
  }),
})
```
