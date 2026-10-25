# Poe (`poe`) Provider Plugin Specification (`opencode`)

## 1. Overview & Identification

- **Plugin ID**: `opencode.provider.poe` (`packages/core/src/plugin/provider/poe.ts`)
- **Integration ID**: `poe` (`methodID: "browser"`)
- **Runtime Package**: `@opencode/ai/providers/openai-compatible`

---

## 2. OAuth PKCE API Key Provisioning Flow (`packages/core/src/plugin/provider/poe.ts`)

| Parameter | Value |
|---|---|
| **Issuer** | `https://poe.com` |
| **Authorization URL** | `https://poe.com/oauth/authorize` |
| **Token Exchange URL** | `POST https://api.poe.com/token` (`Content-Type: application/x-www-form-urlencoded`) |
| **Client ID** | `client_728290227fc048cc9262091a1ea197ea` |
| **Scope** | `apikey:create` |
| **Redirect URI** | `http://127.0.0.1:${port}/callback` (ephemeral local port) |
| **Token Response Shape** | `{ api_key: string, api_key_expires_in?: number \| null }` |

---

## 3. Implementation (`packages/core/src/plugin/provider/poe.ts`)

```typescript
function exchangeCode(http: HttpClient.HttpClient, input: { code: string; redirect: string; verifier: string }) {
  return Effect.gen(function* () {
    const response = yield* http
      .execute(
        HttpClientRequest.post("https://api.poe.com/token").pipe(
          HttpClientRequest.bodyUrlParams({
            grant_type: "authorization_code",
            client_id: clientID,
            code: input.code,
            redirect_uri: input.redirect,
            code_verifier: input.verifier,
          }),
        ),
      )
      .pipe(Effect.mapError(() => new Error("Poe token exchange request failed")))
    const token = yield* HttpClientResponse.schemaBodyJson(Token)(response).pipe(
      Effect.mapError(() => new Error("Invalid Poe token response")),
    )
    const expires =
      token.api_key_expires_in == null ? maxExpiry : (yield* Clock.currentTimeMillis) + token.api_key_expires_in * 1000
    return Credential.OAuth.make({ type: "oauth", methodID, access: token.api_key, refresh: "", expires })
  })
}
```
