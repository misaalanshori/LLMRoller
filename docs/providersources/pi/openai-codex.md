# OpenAI Codex (`openai-codex`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `openai-codex`
- **API Protocol**: `openai-codex-responses` (`POST /codex/responses` or WebSocket `/codex/responses`)
- **Base URL**: `https://chatgpt.com/backend-api`
- **Source Files**:
  - `packages/ai/src/providers/openai-codex.ts`
  - `packages/ai/src/api/openai-codex-responses.ts`
  - `packages/ai/src/auth/oauth/openai-codex.ts`

## 2. Authentication (PKCE & Device Code OAuth)
- **Client ID**: `app_EMoamEEZ73f0CkXaXp7hrann`
- **Endpoints**:
  - Authorize: `https://auth.openai.com/oauth/authorize` (`redirect_uri: http://localhost:1455/auth/callback`, `scope: "openid profile email offline_access"`)
  - Token: `https://auth.openai.com/oauth/token`
  - Device Code: `POST https://auth.openai.com/api/accounts/deviceauth/usercode` and `POST https://auth.openai.com/api/accounts/deviceauth/token`
- **Account ID Extraction**: Extracts `chatgpt_account_id` from JWT claim `https://api.openai.com/auth` and passes `ChatGPT-Account-Id` header.

## 3. Transport & Protocol Quirks (`packages/ai/src/api/openai-codex-responses.ts`)
- **Zstandard Request Compression**: Compresses HTTP SSE request payloads with `zstd` (`level: 3`, `Content-Encoding: zstd`).
- **WebSocket Support**: Supports persistent WebSocket transport with fallback to HTTP SSE if `websocket_connection_limit_reached` or close code `1009` (`WEBSOCKET_MESSAGE_TOO_BIG_CLOSE_CODE`) occurs.
- **Stateful Continuation**: Tracks `previous_response_id` across turns and retries with full transcript if `previous_response_not_found` is returned.

## 4. Source Implementation (`packages/ai/src/providers/openai-codex.ts`)
```typescript
export function openaiCodexProvider(): Provider<"openai-codex-responses"> {
	return createProvider({
		id: "openai-codex",
		name: "OpenAI Codex (legacy)",
		baseUrl: "https://chatgpt.com/backend-api",
		auth: {
			oauth: lazyOAuth({
				name: "OpenAI (ChatGPT Plus/Pro)",
				isSubscription: true,
				load: loadOpenAICodexOAuth,
			}),
		},
		models: Object.values(OPENAI_CODEX_MODELS),
		api: openAICodexResponsesApi(),
	});
}
```
