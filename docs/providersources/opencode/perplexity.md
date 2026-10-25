# Perplexity (`perplexity`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.provider.perplexity` (`packages/core/src/plugin/provider/perplexity.ts`)
- **SDK Package**: `@ai-sdk/perplexity` (`createPerplexity`)

---

## 2. Implementation (`packages/core/src/plugin/provider/perplexity.ts`)

```typescript
import { createProviderPlugin } from "./factory.js"

export const PerplexityPlugin = createProviderPlugin({
  id: "opencode.provider.perplexity",
  package: "@ai-sdk/perplexity",
  load: async (options) => {
    const { createPerplexity } = await import("@ai-sdk/perplexity")
    return createPerplexity(options)
  },
})
```
