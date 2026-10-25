# AWS Bedrock (`bedrock`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `bedrock`
- **Alias**: `br`
- **Aliases**: `["aws-bedrock"]`
- **UI Alias**: `br`
- **Display Name**: AWS Bedrock
- **Category**: `apikey`
- **Credential Form**: `"aws"` (`apiKeyOptionalWith: "profile"`)
- **Website**: `https://aws.amazon.com/bedrock/`
- **Executor**: `BedrockExecutor` (`open-sse/executors/bedrock.js`)
- **Wire Format**: `claude` (Anthropic Messages API payload over AWS Bedrock `/invoke` and AWS EventStream `/invoke-with-response-stream`)

## 2. Endpoints & Model Family Enforcement
- **Streaming Endpoint**: `POST https://bedrock-runtime.{region}.amazonaws.com/model/{escapeUri(model)}/invoke-with-response-stream`
- **Non-Streaming Endpoint**: `POST https://bedrock-runtime.{region}.amazonaws.com/model/{escapeUri(model)}/invoke`
- **Credential Probe Endpoint**: `GET https://bedrock.{region}.amazonaws.com/foundation-models`
- **Model Family Regex Guard**: `BEDROCK.modelFamilyPatterns.claude = /(^|[./])anthropic\./` — rejects non-Anthropic model IDs upfront before billing.
- **URI Escaping**: `escapeUri(model)` escapes the model ID once in `buildUrl()`, and `signAwsRequest()` escapes it a second time for the SigV4 canonical URI (required by Bedrock for `:0` version suffixes).

## 3. Authentication & AWS SigV4 Signing
Resolved per request via `resolveAwsCredentials()` (`open-sse/shared/awsCredentials.js`) and signed via `signAwsRequest()` (`open-sse/utils/awsSigv4.js`):
- **Profile / SSO Mode**: When `providerSpecificData.profile` is set, resolves via `@aws-sdk/credential-providers` (`fromIni({ profile })()`), supporting `aws sso login` and role chaining with a 5-minute pre-expiry refresh lead.
- **Static Mode**: Uses `credentials.apiKey` as `secretAccessKey`, `providerSpecificData.accessKeyId` as `accessKeyId`, and optional `providerSpecificData.sessionToken` (mandatory for `ASIA...` temporary keys).
- **Region Resolution**: `providerSpecificData.region` -> `AWS_REGION` -> `AWS_DEFAULT_REGION` -> `"us-east-1"`.

## 4. Request Transformation & EventStream Decoding
- **Headers**:
  - `Content-Type: application/json`
  - `Accept: application/vnd.amazon.eventstream` (when `stream: true`) or `application/json` (when `stream: false`)
  - AWS SigV4 headers (`Authorization`, `x-amz-date`, `x-amz-security-token`)
- **Body Transformation (`transformRequest`)**:
  ```javascript
  transformRequest(model, body, stream, credentials) {
    const { model: _model, stream: _stream, ...rest } = body || {};
    if (!this.isClaudeWire) return rest;
    return { ...rest, anthropic_version: "bedrock-2023-05-31" };
  }
  ```
- **EventStream Unwrapping (`eventStreamToSse`)**:
  - Validates prelude CRC32 (`view.getUint32(8, false) === crc32(buffer.subarray(0, 8))`) and frame length bounds (`16 <= totalLength <= 24MB`).
  - Handles in-band `:message-type === "exception" | "error"` frames by emitting a Claude SSE `error` event.
  - Base64-decodes `event.payload.bytes` from `:event-type === "chunk"` frames into Anthropic event JSON `inner` and re-emits as:
    ```
    event: ${inner.type}\ndata: ${JSON.stringify(inner)}\n\n
    ```
  - Verifies that `inner.type === "message_stop"` is seen before stream end; otherwise emits an `api_error` truncation event.

## 5. Supported Models (Cross-Region Inference Profiles)
- `us.anthropic.claude-opus-5` ("Claude Opus 5")
- `us.anthropic.claude-sonnet-5` ("Claude Sonnet 5")
- `us.anthropic.claude-fable-5` ("Claude Fable 5")
- `us.anthropic.claude-opus-4-8` ("Claude Opus 4.8")
- `us.anthropic.claude-opus-4-7` ("Claude Opus 4.7")
- `us.anthropic.claude-sonnet-4-6` ("Claude Sonnet 4.6")
- `us.anthropic.claude-opus-4-5-20251101-v1:0` ("Claude Opus 4.5")
- `us.anthropic.claude-sonnet-4-5-20250929-v1:0` ("Claude Sonnet 4.5")
- `us.anthropic.claude-opus-4-1-20250805-v1:0` ("Claude Opus 4.1")
- `us.anthropic.claude-sonnet-4-20250514-v1:0` ("Claude Sonnet 4")
- `us.anthropic.claude-3-haiku-20240307-v1:0` ("Claude 3 Haiku")
- `passthroughModels: true`

## 6. Source Implementation (`open-sse/providers/registry/bedrock.js`)
```javascript
export default {
  id: "bedrock",
  alias: "br",
  aliases: ["aws-bedrock"],
  uiAlias: "br",
  category: "apikey",
  authType: "apikey",
  apiKeyOptionalWith: "profile",
  credentialForm: "aws",
  display: {
    name: "AWS Bedrock",
    icon: "cloud",
    color: "#FF9900",
    textIcon: "BR",
    website: "https://aws.amazon.com/bedrock/",
    notice: {
      text:
        "Two ways to authenticate, both entered in the AWS Bedrock Credentials section below. " +
        "SSO / profile (recommended): fill in Profile and Region, leave the API key empty, then " +
        "run `aws sso login --profile <name>` — credentials refresh automatically. " +
        "Static keys: put the AWS secret access key in the API Key field and the key id in " +
        "Access Key ID, adding Session Token if they are temporary (ASIA…) keys. " +
        "A profile, if set, takes precedence over static keys.",
      apiKeyUrl: "https://console.aws.amazon.com/iam/home#/security_credentials",
    },
  },
  transport: {
    baseUrl: "https://bedrock-runtime.{region}.amazonaws.com",
    format: "claude",
    auth: { header: "aws-sigv4", scheme: "raw" },
  },
  passthroughModels: true,
  models: [
    { id: "us.anthropic.claude-opus-5", name: "Claude Opus 5" },
    { id: "us.anthropic.claude-sonnet-5", name: "Claude Sonnet 5" },
    { id: "us.anthropic.claude-fable-5", name: "Claude Fable 5" },
    { id: "us.anthropic.claude-opus-4-8", name: "Claude Opus 4.8" },
    { id: "us.anthropic.claude-opus-4-7", name: "Claude Opus 4.7" },
    { id: "us.anthropic.claude-sonnet-4-6", name: "Claude Sonnet 4.6" },
    {
      id: "us.anthropic.claude-opus-4-5-20251101-v1:0",
      name: "Claude Opus 4.5",
    },
    {
      id: "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
      name: "Claude Sonnet 4.5",
    },
    {
      id: "us.anthropic.claude-opus-4-1-20250805-v1:0",
      name: "Claude Opus 4.1",
    },
    {
      id: "us.anthropic.claude-sonnet-4-20250514-v1:0",
      name: "Claude Sonnet 4",
    },
    { id: "us.anthropic.claude-3-haiku-20240307-v1:0", name: "Claude 3 Haiku" },
  ],
  hasProviderSpecificData: true,
};
```
