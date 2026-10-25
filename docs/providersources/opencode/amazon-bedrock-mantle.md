# Amazon Bedrock Mantle (`amazon-bedrock-mantle`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `amazon-bedrock` (`providerMetadataKey: "mantle"`)
- **Package Entrypoints**:
  - `@opencode/ai/providers/amazon-bedrock-mantle` (`packages/ai/src/providers/amazon-bedrock-mantle.ts`)
  - `@opencode/ai/providers/amazon-bedrock/mantle`
  - `@opencode/ai/providers/amazon-bedrock/mantle/chat`
  - `@opencode/ai/providers/amazon-bedrock/mantle/messages`
  - `@opencode/ai/providers/amazon-bedrock/mantle/responses`

---

## 2. Endpoints, Routing & Authentication

| Route | Base URL | Path | Protocol |
|---|---|---|---|
| **Responses (`bedrock-mantle-responses`)** | `https://bedrock-mantle.${region}.api.aws/v1` | `POST /responses` | `OpenResponses.protocol` (`store: false`, `include: ["reasoning.encrypted_content"]`) |
| **Chat (`bedrock-mantle-chat`)** | `https://bedrock-mantle.${region}.api.aws/v1` | `POST /chat/completions` | `OpenAIChat.protocol` |
| **Messages (`bedrock-mantle-messages`)** | `https://bedrock-mantle.${region}.api.aws/anthropic/v1` | `POST /messages` | `AnthropicMessages.protocol` (`anthropic-version: 2023-06-01`) |

- **Authentication**: `BedrockAuth.resolveAuth(input, region, { service: "bedrock-mantle", name: "Bedrock Mantle", mode: input.auth })` (supports `AWS_BEARER_TOKEN_BEDROCK` Bearer token or AWS SigV4 with service `"bedrock-mantle"`).
- **Model Routing (`packages/core/src/aisdk-native.ts`)**:
  - `modelID.includes("anthropic.")` -> `@opencode/ai/providers/amazon-bedrock/mantle/messages`
  - `modelID.includes("gpt-oss")` -> `@opencode/ai/providers/amazon-bedrock/mantle/chat`
  - Otherwise -> `@opencode/ai/providers/amazon-bedrock/mantle/responses`

---

## 3. Implementation (`packages/ai/src/providers/amazon-bedrock-mantle.ts`)

```typescript
const configuredRoute = <Body, Prepared>(
  route: Route<Body, Prepared>,
  input: Config,
  defaultBaseURL = (region: string) => `https://bedrock-mantle.${region}.api.aws/v1`,
) => {
  const region = BedrockAuth.resolveRegion(input)
  return route.with({
    endpoint: { baseURL: input.baseURL ?? defaultBaseURL(region) },
    auth: BedrockAuth.resolveAuth(input, region, {
      service: "bedrock-mantle",
      name: "Bedrock Mantle",
      mode: input.auth,
    }),
  })
}
```
