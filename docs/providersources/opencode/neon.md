# Neon AI Gateway (`neon`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `neon`
- **Plugin ID**: `opencode.provider.neon` (`packages/core/src/plugin/provider/neon.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`

---

## 2. Environment Credential Configuration (`packages/core/src/plugin/provider/neon.ts`)

Restricts environment credential auto-detection to `NEON_AI_GATEWAY_TOKEN` (excluding `NEON_AI_GATEWAY_BASE_URL`, which is used only for the base URL template).

```typescript
import { Effect } from "effect"
import { define } from "@opencode/plugin/effect/plugin"

export const NeonPlugin = define({
  id: "opencode.provider.neon",
  effect: Effect.fn(function* (ctx) {
    yield* ctx.integration.transform((editor) => {
      // models.dev also lists NEON_AI_GATEWAY_BASE_URL, which only fills the base URL template.
      editor.method.update({ integrationID: "neon", method: { type: "env", names: ["NEON_AI_GATEWAY_TOKEN"] } })
    })
  }),
})
```
