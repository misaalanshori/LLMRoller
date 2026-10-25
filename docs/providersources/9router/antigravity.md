# Antigravity (`antigravity`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `antigravity`
- **Alias**: `ag`
- **UI Alias**: `ag`
- **Display Name**: Antigravity
- **Category**: `oauth`
- **Service Kinds**: `["llm", "image", "webSearch"]`
- **Website**: `https://antigravity.google`
- **Executor**: `AntigravityExecutor` (`open-sse/executors/antigravity.js`)
- **Image Adapter**: `open-sse/handlers/imageProviders/antigravity.js`
- **Wire Format**: `antigravity` (Google Cloud Code Assist internal `v1internal` envelope wrapping Gemini `generateContent` / `streamGenerateContent`)

## 2. Endpoints & URLs
- **Base URL**: `https://daily-cloudcode-pa.googleapis.com`
- **Streaming Chat Endpoint**: `POST https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse`
- **Non-Streaming Chat / Image Gen / Search Endpoint**: `POST https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent`
- **Quota / Available Models Endpoint**: `POST https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels`
- **Quota Summary Endpoint**: `POST https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary`
- **Load Project / Code Assist Endpoint**: `POST https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist`
- **Onboard User Endpoint**: `POST https://cloudcode-pa.googleapis.com/v1internal:onboardUser`
- **OAuth Authorize URL**: `https://accounts.google.com/o/oauth2/v2/auth`
- **OAuth Token URL**: `https://oauth2.googleapis.com/token`
- **OAuth UserInfo URL**: `https://www.googleapis.com/oauth2/v1/userinfo?alt=json`

## 3. OAuth 2.0 & Onboarding Specification
- **Client ID**: `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com`
- **Client Secret**: `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`
- **Scopes**:
  - `https://www.googleapis.com/auth/cloud-platform`
  - `https://www.googleapis.com/auth/userinfo.email`
  - `https://www.googleapis.com/auth/userinfo.profile`
  - `https://www.googleapis.com/auth/cclog`
  - `https://www.googleapis.com/auth/experimentsandconfigs`
- **Post-Exchange Project & Tier Onboarding**:
  1. Calls `GET https://www.googleapis.com/oauth2/v1/userinfo?alt=json` with `Authorization: Bearer <access_token>` and `x-request-source: local`.
  2. Calls `POST https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist` with `{ metadata }` to extract `cloudaicompanionProject.id` and default `allowedTiers[].id`.
  3. Calls `POST https://cloudcode-pa.googleapis.com/v1internal:onboardUser` with `{ tierId, metadata }` up to 10 times (polling every 5s until `result.done === true`).

## 4. Headers & Client Spoofing
- **User-Agent**: `antigravity/hub/2.24.0 darwin/arm64` (pinned to official Antigravity Hub Desktop 2.24.0 macOS arm64 fingerprint even when running on Linux).
- **Request Headers**:
  - `Content-Type: application/json`
  - `Authorization: Bearer <accessToken>`
  - `User-Agent: antigravity/hub/2.24.0 darwin/arm64`

## 5. Request Envelope & Transformation Specification
### 5.1 IDE Request ID Format
Every request carries a deterministic `requestId` matching `^agent\/[^/]+\/\d+\/[^/]+\/\d+$`:
```
agent/<conversationId>/<timestampMs>/<trajectoryId>/<step>
```
where `conversationId` and `trajectoryId` are UUIDv5-style SHA-256 hashes seeded from `sessionId`, `model`, and `requestType`, and `step = Math.max(1, contents.length * 2 - 1)`.

### 5.2 Standard Chat Request Envelope
```json
{
  "project": "<projectId>",
  "model": "<upstreamModelId>",
  "userAgent": "antigravity",
  "requestId": "agent/<conversationId>/<timestamp>/<trajectoryId>/<step>",
  "request": {
    "contents": [...],
    "systemInstruction": { "parts": [...] },
    "generationConfig": { "maxOutputTokens": 64000, ... },
    "tools": [{ "functionDeclarations": [...] }],
    "toolConfig": { "functionCallingConfig": { "mode": "VALIDATED" } },
    "sessionId": "<numericOrRawSessionId>"
  }
}
```
- **Crucial Quirk**: `requestType` is intentionally **deleted** (`delete body.requestType`) on the chat/agent path because sending `requestType: "agent"` triggers a detail-free 429 `RESOURCE_EXHAUSTED` from Google.
- **Blacklisted Fields Stripped**: `output_config`, `thinking`, `reasoning_effort`, `reasoning`, `enable_thinking`, `thinking_budget`, `thinkingConfig`, and `stream_options` (when `stream !== true`).
- **Thought Signatures**: Gemini 3+ rejects `functionCall` parts without a `thoughtSignature`. `AntigravityExecutor` backfills missing signatures on the first `functionCall` part in a turn from `thoughtSignatureStore` or `DEFAULT_THINKING_AG_SIGNATURE`, while stripping signatures from unsigned sibling parallel calls.
- **System Prompt Rewrites & Tool Cloaking**:
  - Competing client branding in `systemInstruction.parts` is rewritten via `ANTIGRAVITY_PROMPT_REWRITES`.
  - `AntigravityExecutor.cloakTools()` renames client tools with `_ide` suffix (`AG_TOOL_SUFFIX`) and appends 21 decoy native Antigravity tools (`browser_subagent`, `command_status`, `find_by_name`, `generate_image`, `grep_search`, `list_dir`, `run_command`, `view_file`, `write_to_file`, etc.) marked `"This tool is currently unavailable."`.

### 5.3 Image Generation Envelope
When `model` matches `/image|imagen|image-generation/i` (e.g., `gemini-3.1-flash-image`):
- Always forces non-streaming (`generateContent`).
- Parses `-WxH` suffix on model name into `generationConfig.imageConfig.aspectRatio` (e.g. `16:9` or `1:1`) and strips the suffix from `model`.
- Sends `requestType: "image_gen"` with `temperature: 1.0, topP: 0.95, topK: 40, maxOutputTokens: 8192`.

## 6. Supported Models
- `gemini-4-argon-high` -> `gemini-4-argon-high(high)`
- `gemini-4-argon-medium` -> `gemini-4-argon-medium(medium)`
- `gemini-4-argon-low` -> `gemini-4-argon-low(low)`
- `gemini-3.8-flash-high` -> `gemini-3.8-flash-high(high)`
- `gemini-3.8-flash-medium` -> `gemini-3.8-flash-medium(medium)`
- `gemini-3.8-flash-low` -> `gemini-3.8-flash-low(low)`
- `gemini-3.8-flash` -> `gemini-3.8-flash-medium(medium)`
- `gemini-3.7-flash-high` / `medium` / `low` -> `gemini-3.7-flash-tiered(high|medium|low)`
- `gemini-3.6-flash-high` / `medium` / `low` -> `gemini-3.6-flash-tiered(high|medium|low)`
- `gemini-3.1-pro-high` / `gemini-pro-agent` -> `gemini-pro-agent`
- `gemini-3.1-pro-low` -> `gemini-3.1-pro-low(low)`
- `claude-sonnet-5-5-high` / `medium` / `low` / `claude-sonnet-5-5`
- `claude-opus-5-5-high` / `medium` / `low` / `claude-opus-5-5`
- `claude-sonnet-4-6`, `claude-opus-4-6-thinking`, `gpt-oss-120b-medium`
- `gemini-3.1-flash-image` (Image generation)

## 7. Source Implementation Snippets

### Executor Request Transformation (`open-sse/executors/antigravity.js`)
```javascript
function buildIdeRequestId({ body, request, credentials, model, requestType }) {
  if (ANTIGRAVITY_IDE_REQUEST_ID_RE.test(body?.requestId || "")) {
    return body.requestId;
  }

  const sessionId = request?.sessionId || body?.request?.sessionId || credentials?._clientSessionId || credentials?.connectionId || credentials?.email || "anonymous";
  const conversationId = uuidFromSeed(`antigravity:conversation:${sessionId}`);
  const trajectoryId = uuidFromSeed(`antigravity:trajectory:${sessionId}:${model}:${requestType}`);
  const contentCount = Array.isArray(request?.contents) ? request.contents.length : 1;
  const step = Math.max(1, contentCount * 2 - 1);
  return `agent/${conversationId}/${Date.now()}/${trajectoryId}/${step}`;
}
```
