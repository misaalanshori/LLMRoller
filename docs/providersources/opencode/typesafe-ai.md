# TypeSafe AI (`typesafe-ai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `typesafe-ai`
- **Package Entrypoint**: `packages/ai/src/providers/typesafe-ai.ts`
- **Protocol**: `SystemOne.model` (`packages/ai/src/experimental/system-one.ts`)

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.typesafe.ai/v1` |
| **System One Evaluation Path** | `POST https://api.typesafe.ai/v1/systemone` |
| **Authentication** | `Authorization: Bearer <apiKey>` (env fallback `TYPESAFE_API_KEY`) |
| **Rounding** | `SystemOne.jevRounding` (`probabilityDecimals: 2`, `scoreDecimals: 2`) |

---

## 3. Implementation (`packages/ai/src/providers/typesafe-ai.ts`)

```typescript
import { HttpOptions, ProviderID, type ModelID } from "../schema/index.js"
import { AuthOptions, type ProviderAuthOption } from "../route/auth-options.js"
import { SystemOne } from "../experimental/system-one.js"

export const id = ProviderID.make("typesafe-ai")
const baseURL = "https://api.typesafe.ai/v1"

export const configure = (input: Options = {}) => {
  const evaluation = (modelID: string | ModelID) =>
    SystemOne.model({
      id: modelID,
      provider: id,
      providerMetadataKey: "typesafe",
      auth: AuthOptions.bearer(input, "TYPESAFE_API_KEY"),
      baseURL: input.baseURL ?? baseURL,
      headers: input.headers,
      http: HttpOptions.make(input.http),
      rounding: SystemOne.jevRounding,
    })
  return { id, experimental: { evaluation }, configure }
}
```
