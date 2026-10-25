# Amazon Bedrock (`amazon-bedrock`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `amazon-bedrock`
- **Display Name**: `Amazon Bedrock`
- **Provider Module**: `packages/ai/src/providers/amazon-bedrock.ts`
- **API Implementation**: `packages/ai/src/api/bedrock-converse-stream.ts` (`bedrock-converse-stream`)
- **Models Catalog**: `packages/ai/src/providers/amazon-bedrock.models.ts` (`AMAZON_BEDROCK_MODELS`)

---

## 2. Authentication & Credential Chain (`packages/ai/src/providers/amazon-bedrock.ts`)

Supports 3 interactive login modes and 6 ambient credential sources via `@aws-sdk/client-bedrock-runtime` (`BedrockRuntimeClient` + `ConverseStreamCommand`):
1. **Bearer Token (`bearer-token`)**: Stored `credential.key` or `AWS_BEARER_TOKEN_BEDROCK` env var -> bypasses SigV4 signing and sends `Authorization: Bearer <token>` via custom Smithy `finalizeRequest` middleware.
2. **AWS Profile (`aws-profile`)**: `AWS_PROFILE` env var or stored `credential.env.AWS_PROFILE`.
3. **AWS Credential Chain (`credential-chain`)**:
   - `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`
   - `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` / `AWS_CONTAINER_CREDENTIALS_FULL_URI` (ECS task roles)
   - `AWS_WEB_IDENTITY_TOKEN_FILE` (IRSA)

```typescript
export function amazonBedrockProvider(): Provider<"bedrock-converse-stream"> {
	return createProvider({
		id: "amazon-bedrock",
		name: "Amazon Bedrock",
		auth: { apiKey: bedrockAuth },
		models: Object.values(AMAZON_BEDROCK_MODELS),
		api: bedrockConverseStreamApi(),
	});
}
```

---

## 3. API & Request Handling (`packages/ai/src/api/bedrock-converse-stream.ts`)

- **Default Region**: `options.region || env.AWS_REGION || env.AWS_DEFAULT_REGION || "us-east-1"`
- **Custom Endpoint Override**: `env.AWS_ENDPOINT_URL_BEDROCK_RUNTIME`
- **Proxy Support**: Uses `NodeHttpHandler` with `HttpProxyAgent` / `HttpsProxyAgent` when `HTTP_PROXY` / `HTTPS_PROXY` is set.
- **Thinking & Beta Flags**:
  - Supports `thinkingDisplay: "summarized" | "omitted"` (defaults to `"summarized"`).
  - Injects `thinking-binding-controls-2026-08-01` and `interleaved-thinking-2025-05-14` into `additionalModelRequestFields.anthropic_beta` for Claude models.
  - Supports `CacheTTL.ONE_HOUR` when `cacheRetention === "long"` and `model.compat?.supportsLongCacheRetention` is true.
