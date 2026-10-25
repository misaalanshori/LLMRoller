# Google Antigravity (`antigravity`) Provider Specification (`pi-antigravity`)

## 1. Overview
- **Provider ID**: `antigravity`
- **Provider Name**: `Antigravity`
- **API Protocols**:
  - Chat / Agent Streaming: `google-antigravity` (`ANTIGRAVITY_API` via `streamAntigravity` in `src/stream/stream.ts`)
  - Image Generation: `antigravity-images` (`ANTIGRAVITY_IMAGE_API` via `generateAntigravityImages` in `src/image/native.ts`)
  - Web Search Grounding: `/antigravity.search` (`executeAntigravitySearch` in `src/search/search.ts`)
- **Primary Base URL & Fallback Endpoints** (`src/client/client.ts`):
  1. `https://daily-cloudcode-pa.googleapis.com` (`DEFAULT_ENDPOINT`)
  2. `https://daily-cloudcode-pa.sandbox.googleapis.com`
  3. `https://cloudcode-pa.googleapis.com`
  - Overrideable via `ANTIGRAVITY_BASE_URL` or `NOAGY_BASE_URL` (validated by `assertSafeApiBaseUrl`).
- **Source Files**:
  - `src/index.ts`
  - `src/auth/oauth.ts`, `src/auth/accounts.ts`
  - `src/client/client.ts`
  - `src/models/models.ts`, `src/models/discovery.ts`, `src/models/grouping.ts`
  - `src/stream/stream.ts`
  - `src/image/image.ts`, `src/image/native.ts`
  - `src/search/search.ts`
  - `src/usage/usage.ts`
  - `src/utils/util.ts`

## 2. Authentication (Google OAuth 2.0 PKCE + Multi-Account Failover)
- **OAuth Endpoints (`src/auth/oauth.ts`)**:
  - **Authorization URL**: `https://accounts.google.com/o/oauth2/v2/auth`
  - **Token URL**: `https://oauth2.googleapis.com/token`
  - **UserInfo URL**: `https://www.googleapis.com/oauth2/v1/userinfo?alt=json`
  - **Redirect URI**: `http://localhost:51121/oauth-callback`
  - **Client ID**: `1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com` (or `ANTIGRAVITY_CLIENT_ID`)
  - **Client Secret**: `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` (or `ANTIGRAVITY_CLIENT_SECRET`)
  - **Scopes**:
    - `https://www.googleapis.com/auth/aicode`
    - `https://www.googleapis.com/auth/cloud-platform`
    - `https://www.googleapis.com/auth/userinfo.email`
    - `https://www.googleapis.com/auth/userinfo.profile`
    - `https://www.googleapis.com/auth/cclog`
    - `https://www.googleapis.com/auth/experimentsandconfigs`
- **Project Discovery (`loadCodeAssist` & `listCloudAICompanionProjects`)**:
  - Calls `POST {endpoint}/v1internal:loadCodeAssist` with `{"metadata": {"ideType": "ANTIGRAVITY"}}`.
  - Extracts `antigravityProjectId`, `projectId`, `backendProjectId`, `userDefinedCloudaicompanionProject`, or `cloudaicompanionProject`.
  - Falls back to `POST {endpoint}/v1internal:listCloudAICompanionProjects` or deterministic UUIDv5 derived from `antigravity:<email>`.
- **Serialized API Key Credential**:
  - Stored as JSON string `{"token": "<access_token>", "projectId": "<project_id>"}` and parsed via `parseApiKey()`.
  - Supports multi-account rotation and automatic failover on 429 quota exhaustion (`failoverToNextAccount` in `src/auth/accounts.ts`).

## 3. HTTP Headers & Wire Fingerprint
- **Default User-Agent**:
  `antigravity/cli/1.4.0 (aidev_client; os_type=linux; arch=amd64; cl=982146307; auth_method=consumer)`
  *(Note: Catalog visibility for Gemini 3.8 / Gemini 4 Argon is gated on CLI version `>= 1.4.0`)*
- **Standard Request Headers (`antigravityHeaders`)**:
  ```http
  Authorization: Bearer <oauth_access_token>
  Content-Type: application/json
  User-Agent: antigravity/cli/1.4.0 (aidev_client; os_type=linux; arch=amd64; cl=982146307; auth_method=consumer)
  ```
- **Streaming SSE Request Headers**:
  Adds `Accept: text/event-stream`.

## 4. Upstream RPC Endpoints
| Operation | HTTP Method & Path | Request Payload |
| :--- | :--- | :--- |
| **Project Discovery** | `POST /v1internal:loadCodeAssist` | `{"metadata": {"ideType": "ANTIGRAVITY"}}` |
| **Fallback Project List** | `POST /v1internal:listCloudAICompanionProjects` | `{}` |
| **Model Catalog Discovery** | `POST /v1internal:fetchAvailableModels` | `{"project": "<projectId>"}` |
| **Agent Chat Stream (SSE)** | `POST /v1internal:streamGenerateContent?alt=sse` | `AntigravityGenerateRequest` envelope |
| **Non-Streaming / Search / Image** | `POST /v1internal:generateContent` or `:streamGenerateContent?alt=sse` | `AntigravityGenerateRequest` envelope |
| **Quota & Tier Lookup** | `POST /v1internal:retrieveUserQuota` / `:loadCodeAssist` | `{"project": "<projectId>"}` |

## 5. Request Envelope & Protocol Quirks (`src/stream/stream.ts` & `src/utils/util.ts`)
- **Outer Request Envelope (`AntigravityGenerateRequest`)**:
  ```json
  {
    "project": "<projectId>",
    "model": "<runtimeModelId>",
    "requestType": "agent",
    "userAgent": "antigravity",
    "requestId": "agent/<conversationId>/<timestampMs>/<trajectoryId>/<step>",
    "request": {
      "contents": [...],
      "systemInstruction": { "role": "user", "parts": [...] },
      "generationConfig": {
        "temperature": 0.7,
        "maxOutputTokens": 65536,
        "thinkingConfig": { "includeThoughts": true, "thinkingLevel": "HIGH" }
      },
      "tools": [{ "functionDeclarations": [...] }],
      "toolConfig": { "functionCallingConfig": { "mode": "AUTO" } },
      "sessionId": "<signed-int64-string>",
      "labels": {
        "last_step_index": "0",
        "request_id": "<trajectoryId>-0",
        "trajectory_id": "<trajectoryId>",
        "used_claude": "false",
        "used_claude_conservative": "false",
        "used_non_gemini_model": "false",
        "model_enum": "MODEL_...",
        "last_execution_id": "<uuid-on-step-gt-1>"
      }
    }
  }
  ```
- **Model Routing by Thinking Effort (`ANTIGRAVITY_ROUTING`)**:
  - `claude-opus-4-6` -> `claude-opus-4-6-thinking`
  - `claude-sonnet-4-6` -> `claude-sonnet-4-6`
  - `gemini-3.1-pro` -> `gemini-3.1-pro-low` (off/minimal/low/medium), `gemini-pro-agent` (high/xhigh)
  - `gemini-3.8-flash` -> `gemini-3.8-flash-low` / `-medium` / `-high`
  - `gemini-3.7-flash` -> `gemini-3.7-flash-low` / `-medium` / `-high`
  - `gemini-3.6-flash` -> `gemini-3.6-flash-low` / `-medium` / `-high`
  - `gemini-3.5-flash` -> `gemini-3.5-flash-extra-low` / `-low` / `gemini-3-flash-agent`
  - `gpt-oss-120b` -> `gpt-oss-120b-medium`
- **Tool Schema Dual-Bridge**:
  - Gemini models receive dereferenced JSON Schema (with `$schema`, `$defs`, `definitions` stripped) in `parametersJsonSchema`.
  - Claude (`claude-*`) and GPT-OSS (`gpt-oss-*`) models use Cloud Code Assist's custom-tool bridge in `parameters`, filtered strictly to the Protobuf `Schema` allowlist (`type`, `description`, `properties`, `required`, `items`, `enum`).
- **Thought Signature Enforcement**:
  - Gemini 3+ models require valid base64 `thoughtSignature` on replayed `functionCall` turns. If a prior assistant tool call lacks a valid signature (e.g. from another model/provider), `convertMessages` converts the tool call/result pair into a synthetic user observation `[Observation from \`toolName\` (...): ...]`.

## 6. Source Implementation (`src/client/client.ts` & `src/stream/stream.ts`)
```typescript
export const DEFAULT_ENDPOINT = "https://daily-cloudcode-pa.googleapis.com";
export const ENDPOINT_FALLBACKS = [
  DEFAULT_ENDPOINT,
  "https://daily-cloudcode-pa.sandbox.googleapis.com",
  "https://cloudcode-pa.googleapis.com",
];

const DEFAULT_USER_AGENT =
  "antigravity/cli/1.4.0 (aidev_client; os_type=linux; arch=amd64; cl=982146307; auth_method=consumer)";

export function antigravityHeaders(token: string): Record<string, string> {
  return {
    Authorization: `Bearer ${token}`,
    "Content-Type": "application/json",
    "User-Agent": antigravityEnv("USER_AGENT") || defaultUserAgent(),
  };
}
```
