# AWS Bedrock (xAI) (`bedrock-xai`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `bedrock-xai`
- **Alias**: `brx`
- **Aliases**: `["bedrock-grok"]`
- **UI Alias**: `brx`
- **Display Name**: AWS Bedrock (xAI)
- **Category**: `apikey`
- **Credential Form**: `"aws"` (`apiKeyOptionalWith: "profile"`)
- **Website**: `https://aws.amazon.com/bedrock/`
- **Executor**: `new BedrockExecutor("bedrock-xai")` (`open-sse/executors/bedrock.js`)
- **Wire Format**: `openai` (OpenAI Chat Completions over AWS Bedrock `/invoke` and AWS EventStream `/invoke-with-response-stream`)

## 2. Endpoints & Model Family Enforcement
- **Streaming Endpoint**: `POST https://bedrock-runtime.{region}.amazonaws.com/model/{escapeUri(model)}/invoke-with-response-stream`
- **Non-Streaming Endpoint**: `POST https://bedrock-runtime.{region}.amazonaws.com/model/{escapeUri(model)}/invoke`
- **Credential Probe Endpoint**: `GET https://bedrock.{region}.amazonaws.com/foundation-models`
- **Model Family Regex Guard**: `BEDROCK.modelFamilyPatterns.openai = /(^|[./])xai\./` — `BedrockExecutor.buildUrl()` validates that `model` matches `(^|[./])xai\.` before making a request, throwing an error upfront if an incompatible model family is passed.

## 3. Authentication & AWS SigV4 Signing
Supports two credential modes via `open-sse/shared/awsCredentials.js`:
1. **Profile / SSO Mode (`providerSpecificData.profile`)**:
   - Validates profile name against `/^[\w@+.%: -]{1,64}$/`.
   - Resolves credentials lazily via `@aws-sdk/credential-providers` `fromIni({ profile })()`.
   - Caches resolved profile credentials until `expiration - 5 minutes` (or 60s TTL if no expiration).
2. **Static Keys Mode**:
   - `credentials.apiKey` = AWS Secret Access Key
   - `credentials.providerSpecificData.accessKeyId` = AWS Access Key ID (if starts with `ASIA`, `sessionToken` is mandatory)
   - `credentials.providerSpecificData.sessionToken` = optional AWS STS session token
- **Region Resolution Precedence**:
  1. `credentials.providerSpecificData.region`
  2. `process.env.AWS_REGION`
  3. `process.env.AWS_DEFAULT_REGION`
  4. Default: `"us-east-1"`
  (Validated against `/^[a-z0-9][a-z0-9-]{0,30}$/`).
- **SigV4 Request Signing (`open-sse/utils/awsSigv4.js`)**:
  - Service name: `"bedrock"`
  - Algorithm: `AWS4-HMAC-SHA256`
  - Headers signed: `Content-Type`, `Accept`, `host`, `x-amz-date`, and `x-amz-security-token` (when present). `redirect: "error"` is enforced on `fetch` so signed tokens are never replayed on redirects.

## 4. Request & EventStream-to-OpenAI-SSE Protocol
- **Request Headers**:
  - `Content-Type: application/json`
  - `Accept: application/vnd.amazon.eventstream` (streaming) or `application/json` (non-streaming)
- **Request Body Transformation (`transformRequest`)**:
  - Strips `model` and `stream` from the OpenAI request JSON body (since Bedrock takes `model` in the URL path and selects streaming via endpoint path).
  - Unlike `bedrock` (Anthropic wire), `bedrock-xai` (`this.isClaudeWire === false`) does **not** inject `anthropic_version`.
- **EventStream Unwrapping (`eventStreamToSse`)**:
  - Parses binary AWS EventStream frames (verifying prelude CRC32 and frame bounds up to 24 MB).
  - For `:event-type === "chunk"`, base64-decodes `event.payload.bytes` into JSON `inner`.
  - Verifies `Array.isArray(inner?.choices)` and emits standard OpenAI SSE lines: `data: ${JSON.stringify(inner)}\n\n`.
  - Tracks completion when `inner.choices.some(c => c?.finish_reason)` is truthy, and synthesizes `data: [DONE]\n\n` (`SSE_DONE`) at stream close.

## 5. Supported Models
- `us.xai.grok-4.6` ("Grok 4.6")
- `global.xai.grok-4.6` ("Grok 4.6 (global)")
- `passthroughModels: true` (any `*.xai.*` inference profile ID is accepted).

## 6. Source Implementation (`open-sse/providers/registry/bedrock-xai.js`)
```javascript
export default {
  id: "bedrock-xai",
  alias: "brx",
  aliases: ["bedrock-grok"],
  uiAlias: "brx",
  category: "apikey",
  authType: "apikey",
  apiKeyOptionalWith: "profile",
  credentialForm: "aws",
  display: {
    name: "AWS Bedrock (xAI)",
    icon: "cloud",
    color: "#FF9900",
    textIcon: "BX",
    website: "https://aws.amazon.com/bedrock/",
    notice: {
      text:
        "xAI Grok models hosted on AWS Bedrock. Authenticate exactly like the AWS Bedrock " +
        "provider: fill in Profile and Region and leave the API key empty, then run " +
        "`aws sso login --profile <name>`; or use static keys with the AWS secret access key " +
        "as the API key. Note Grok is a reasoning model and spends output budget thinking " +
        'before it answers — a small max_tokens returns finish_reason "length" with empty ' +
        "content, so allow a few thousand tokens.",
      apiKeyUrl:
        "https://console.aws.amazon.com/iam/home#/security_credentials",
    },
  },
  transport: {
    baseUrl: "https://bedrock-runtime.{region}.amazonaws.com",
    auth: { header: "aws-sigv4", scheme: "raw" },
  },
  passthroughModels: true,
  models: [
    { id: "us.xai.grok-4.6", name: "Grok 4.6" },
    { id: "global.xai.grok-4.6", name: "Grok 4.6 (global)" },
  ],
  hasProviderSpecificData: true,
};
```
