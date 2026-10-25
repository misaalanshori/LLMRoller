# ZenMux (`zenmux`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.provider.zenmux` (`packages/core/src/plugin/provider/zenmux.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`
- **Target Base URL**: `https://zenmux.ai/api/v1`

---

## 2. Attribution Headers & Implementation (`packages/core/src/plugin/provider/zenmux.ts`)

Injects attribution headers onto any `@opencode/ai/providers/openai-compatible` provider whose `settings.baseURL` equals `"https://zenmux.ai/api/v1"`:
- `HTTP-Referer: https://opencode.ai/`
- `X-Title: opencode`

```typescript
import { Effect } from "effect"
import { define } from "@opencode/plugin/effect/plugin"

export const ZenmuxPlugin = define({
  id: "opencode.provider.zenmux",
  effect: Effect.fn(function* (ctx) {
    yield* ctx.provider.transform((evt) => {
      for (const item of evt.list()) {
        if (item.provider.package !== "@opencode/ai/providers/openai-compatible") continue
        if (item.provider.settings?.baseURL !== "https://zenmux.ai/api/v1") continue
        evt.update(item.provider.id, (provider) => {
          provider.headers = {
            "HTTP-Referer": "https://opencode.ai/",
            "X-Title": "opencode",
            ...provider.headers,
          }
        })
      }
    })
  }),
})
```
