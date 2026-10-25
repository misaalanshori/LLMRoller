# Amazon Bedrock Converse (`amazon-bedrock`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `amazon-bedrock`
- **Package Entrypoint**: `@opencode/ai/providers/amazon-bedrock` (`packages/ai/src/providers/amazon-bedrock.ts`)
- **Protocols & Helpers**:
  - `packages/ai/src/protocols/bedrock-converse.ts` (`bedrock-converse`)
  - `packages/ai/src/protocols/bedrock-event-stream.ts`
  - `packages/ai/src/protocols/utils/bedrock-auth.ts`
  - `packages/ai/src/protocols/utils/bedrock-cache.ts`
  - `packages/ai/src/protocols/utils/bedrock-media.ts`
- **Core Plugin**: `packages/core/src/plugin/provider/amazon-bedrock.ts` (`opencode.provider.amazon.bedrock`)

---

## 2. Endpoints & Authentication (`packages/ai/src/providers/amazon-bedrock.ts` & `bedrock-auth.ts`)

| Parameter | Value |
|---|---|
| **Base URL** | `https://bedrock-runtime.${region}.amazonaws.com` (default `region = "us-east-1"`) |
| **Streaming Endpoint** | `POST /model/${encodeURIComponent(body.modelId)}/converse-stream` |
| **Framing** | AWS binary event stream (`BedrockEventStream.framing("bedrock-converse")`) |
| **Auth Precedence** | 1. **Bearer Token** (`Authorization: Bearer <apiKey>`, env `AWS_BEARER_TOKEN_BEDROCK` unless `auth === "sigv4"`).<br>2. **Static SigV4** (`aws4fetch` `AwsV4Signer` with `service: "bedrock"`).<br>3. **AWS Default Credential Chain** (`@aws-sdk/credential-providers` `fromNodeProviderChain({ ignoreCache: true, profile })`). |

---

## 3. Request & Protocol Implementation (`packages/ai/src/protocols/bedrock-converse.ts`)

```typescript
export const route = Route.make({
  id: ADAPTER,
  provider: "bedrock",
  providerMetadataKey: "bedrock",
  protocol,
  endpoint: Endpoint.path<BedrockConverseBody>(
    ({ body }) => `/model/${encodeURIComponent(body.modelId)}/converse-stream`,
  ),
  auth: BedrockAuth.auth,
  framing,
})
```

### Key Request Lowering Rules
- **Cache Breakpoints**: Caps `cachePoint: { type: "default" }` markers at 4 (`BedrockCache.BEDROCK_BREAKPOINT_CAP`), allocated in `tools -> system -> messages` order.
- **Thinking Block Binding**: For Claude 5.1+ models (`supportsThinkingBlockBinding`), injects `thinking.block_binding = { prefix_mismatch_behavior: "drop_block" }` and `additionalModelRequestFields.anthropic_beta = [THINKING_BINDING_BETA]`.
- **Nova 2 High Reasoning Effort**: Drops `inferenceConfig.maxTokens` when `amazon.nova-2-*` is called with `additionalModelRequestFields.reasoningConfig.maxReasoningEffort === "high"`.
- **Tool Result Images**: Keeps tool-result images inline for `anthropic.claude-*` and `amazon.nova-*`; hoists tool-result images into the trailing `user` message for other models.
