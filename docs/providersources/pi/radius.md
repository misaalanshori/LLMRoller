# Radius (`radius`) Provider Specification (`pi`)

## 1. Overview
- **Provider ID**: `radius`
- **Name**: Radius
- **API Protocol**: `pi-messages` (`POST /v1/messages` or `<baseUrl>/messages` streaming native `PiMessagesEvent` SSE)
- **Default Gateway**: `https://radius.pi.dev`
- **Source Files**:
  - `packages/ai/src/providers/radius.ts`
  - `packages/ai/src/providers/radius-config.ts`
  - `packages/ai/src/api/pi-messages.ts`
  - `packages/ai/src/auth/oauth/radius.ts`

## 2. Authentication (API Key & OAuth PKCE / Device Code)
1. **API Key**: `RADIUS_API_KEY` (`Authorization: Bearer <RADIUS_API_KEY>`).
2. **OAuth (`packages/ai/src/auth/oauth/radius.ts`)**:
   - **Discovery**: `GET <gateway>/v1/oauth` -> `{ authorizationEndpoint }`
   - **Client ID**: `pi-gateway`, **Scope**: `gateway offline_access`
   - **Browser Callback**: `http://127.0.0.1:1456/oauth/callback`
   - **Device Code**: `urn:ietf:params:oauth:grant-type:device_code`

## 3. Native `pi-messages` Wire Protocol (`packages/ai/src/api/pi-messages.ts`)
- Sends `POST <baseUrl>/messages` with `{ model, context, options }`.
- Receives SSE events of serialized `PiMessagesEvent` (`start`, `text_start`, `text_delta`, `text_end`, `thinking_start`, `thinking_delta`, `thinking_end`, `toolcall_start`, `toolcall_delta`, `toolcall_end`, `done`, `error`), including optional `rewrite` (`PiMessagesRewriteImpact` for gateway policy rewrites).

## 4. Source Implementation (`packages/ai/src/providers/radius.ts`)
```typescript
export function radiusProvider(options: RadiusProviderOptions = {}): Provider<"pi-messages"> {
	const id = options.id ?? "radius";
	const name = options.name ?? "Radius";
	const gateway = normalizeRadiusGatewayUrl(options.gateway ?? DEFAULT_RADIUS_GATEWAY);
	const baselineModels: Model<"pi-messages">[] =
		gateway === normalizeRadiusGatewayUrl(DEFAULT_RADIUS_GATEWAY)
			? Object.values(RADIUS_MODELS).map((model) => ({ ...model, provider: id }))
			: [];
	let dynamicModels: Model<"pi-messages">[] | undefined;
	const streams = piMessagesApi();

	return {
		id,
		name,
		auth: {
			apiKey: envApiKeyAuth("Radius API key", ["RADIUS_API_KEY"]),
			oauth: lazyOAuth({ name, load: () => loadRadiusOAuth({ name, gateway }) }),
		},
		getModels: () => dynamicModels ?? baselineModels,
		// ...
	};
}
```
