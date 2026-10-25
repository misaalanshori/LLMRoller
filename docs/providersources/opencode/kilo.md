# Kilo Gateway (`kilo`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.provider.kilo` (`packages/core/src/plugin/provider/kilo.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`
- **Target Base URL**: `https://api.kilo.ai/api/gateway`

---

## 2. Attribution Headers & Implementation (`packages/core/src/plugin/provider/kilo.ts`)

Injects OpenRouter-style attribution headers onto any `@opencode/ai/providers/openai-compatible` provider whose `settings.baseURL` equals `"https://api.kilo.ai/api/gateway"`:
- `HTTP-Referer: https://opencode.ai/`
- `X-Title: opencode`

```typescript
import { Effect } from "effect"
import { define } from "@opencode/plugin/effect/plugin"

export const KiloPlugin = define({
  id: "opencode.provider.kilo",
  effect: Effect.fn(function* (ctx) {
    yield* ctx.provider.transform((evt) => {
      for (const item of evt.list()) {
        if (item.provider.package !== "@opencode/ai/providers/openai-compatible") continue
        if (item.provider.settings?.baseURL !== "https://api.kilo.ai/api/gateway") continue
        evt.update(item.provider.id, (provider) => {
          provider.headers = { ...provider.headers, "HTTP-Referer": "https://opencode.ai/", "X-Title": "opencode" }
        })
      }
    })
  }),
})
```
