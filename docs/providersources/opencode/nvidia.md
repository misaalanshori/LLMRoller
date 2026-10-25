# NVIDIA NIM (`nvidia`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `nvidia`
- **Plugin ID**: `opencode.provider.nvidia` (`packages/core/src/plugin/provider/nvidia.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`
- **Target Base URL**: `https://integrate.api.nvidia.com/v1`

---

## 2. Custom Headers & Implementation (`packages/core/src/plugin/provider/nvidia.ts`)

Injects attribution and billing headers when `settings.baseURL === "https://integrate.api.nvidia.com/v1"`:
- `HTTP-Referer: https://opencode.ai/`
- `X-Title: opencode`
- `X-BILLING-INVOKE-ORIGIN: OpenCode` (unless already overridden in `provider.headers`)

```typescript
import { Effect } from "effect"
import { define } from "@opencode/plugin/effect/plugin"

export const NvidiaPlugin = define({
  id: "opencode.provider.nvidia",
  effect: Effect.fn(function* (ctx) {
    yield* ctx.provider.transform((evt) => {
      for (const item of evt.list()) {
        if (item.provider.package !== "@opencode/ai/providers/openai-compatible") continue
        if (item.provider.settings?.baseURL !== "https://integrate.api.nvidia.com/v1") continue
        evt.update(item.provider.id, (provider) => {
          provider.headers = {
            ...provider.headers,
            "HTTP-Referer": "https://opencode.ai/",
            "X-Title": "opencode",
            "X-BILLING-INVOKE-ORIGIN": provider.headers?.["X-BILLING-INVOKE-ORIGIN"] ?? "OpenCode",
          }
        })
      }
    })
  }),
})
```
