# Snowflake Cortex (`snowflake-cortex`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `snowflake-cortex`
- **Plugin ID**: `opencode.provider.snowflake.cortex` (`packages/core/src/plugin/provider/snowflake-cortex.ts`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`

---

## 2. Endpoints & OAuth PKCE / PAT Authentication

| Parameter | Value |
|---|---|
| **Account Issuer** | `https://${account}.snowflakecomputing.com` |
| **Cortex Inference Base URL** | `https://${account}.snowflakecomputing.com/api/v2/cortex/v1` |
| **OAuth Authorize URL** | `https://${account}.snowflakecomputing.com/oauth/authorize` (`client_id: "LOCAL_APPLICATION"`, `scope: "refresh_token [session:role:<role>]"`, `code_challenge_method: "S256"`) |
| **OAuth Token / Refresh URL** | `POST https://${account}.snowflakecomputing.com/oauth/token-request` (`Authorization: Basic ${base64("LOCAL_APPLICATION:LOCAL_APPLICATION")}`) |
| **Environment Overrides** | `SNOWFLAKE_CORTEX_TOKEN`, `SNOWFLAKE_CORTEX_PAT`, `SNOWFLAKE_ACCOUNT` |

---

## 3. Compatibility & Response Quirks (`packages/core/src/plugin/provider/snowflake-cortex.ts`)

1. **Max Tokens & Claude Caching**:
   - Sets `maxTokensField: "max_completion_tokens"` on all Cortex models.
   - Sets `cacheFormat: "anthropic"` on Cortex models whose ID includes `"claude"`.
2. **HTTP 400 `"conversation complete"` Workaround**:
   - When Snowflake Cortex returns HTTP 400 with `"conversation complete"`, rewrites the response into a synthetic SSE stop stream (`data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n`).
3. **401 Token Refresh Retry**:
   - On HTTP 401 (`attempt === 2`), refreshes the OAuth credential via `/oauth/token-request` and retries immediately (`delay: 0`).
