# LLMGateway (`llmgateway`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.provider.llmgateway` (`packages/core/src/plugin/provider/llmgateway.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`
- **Target Base URL**: `https://api.llmgateway.io/v1`

---

## 2. Attribution Headers & Implementation (`packages/core/src/plugin/provider/llmgateway.ts`)

Injects attribution headers when `settings.baseURL === "https://api.llmgateway.io/v1"`:
- `HTTP-Referer: https://opencode.ai/`
- `X-Title: opencode`
- `X-Source: opencode`

```typescript
import { Effect } from "effect"
import { define } from "@opencode/plugin/effect/plugin"
import { Integration } from "../../integration.js"

export const LLMGatewayPlugin = define({
  id: "opencode.provider.llmgateway",
  effect: Effect.fn(function* (ctx) {
    const integrations = yield* Integration.Service
    const configured = new Set((yield* integrations.list()).map((integration) => integration.id))
    yield* ctx.provider.transform((evt) => {
      for (const item of evt.list()) {
        if (item.provider.activation === "disabled") continue
        if (item.provider.package !== "@opencode/ai/providers/openai-compatible") continue
        if (item.provider.settings?.baseURL !== "https://api.llmgateway.io/v1") continue
        if (!configured.has(Integration.ID.make(item.provider.id))) continue
        evt.update(item.provider.id, (provider) => {
          provider.headers = {
            ...provider.headers,
            "HTTP-Referer": "https://opencode.ai/",
            "X-Title": "opencode",
            "X-Source": "opencode",
          }
        })
      }
    })
  }),
})
```
