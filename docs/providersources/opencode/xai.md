# xAI (`xai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `xai`
- **Package Entrypoint**: `@opencode/ai/providers/xai` (`packages/ai/src/providers/xai.ts`)
- **Protocols**:
  - `packages/ai/src/protocols/xai-responses.ts` (`xai-responses`)
  - `packages/ai/src/protocols/xai-images.ts` (`xai-images`)
  - `packages/ai/src/protocols/xai-video.ts` (`xai-video`)
  - `OpenAIChat.protocol` (`xai-chat`)
- **Core Plugin**: `packages/core/src/plugin/provider/xai.ts` (`opencode.provider.xai`)

---

## 2. Endpoints, WebSocket Transport & OAuth Device Flow

| Service | Endpoint |
|---|---|
| **Base URL** | `https://api.x.ai/v1` |
| **Responses API (`xai-responses`, Default)** | `POST /responses` (supports WebSocket channel transport rotating every 24m; compaction at `POST /responses/compact`) |
| **Chat Completions (`xai-chat`)** | `POST /chat/completions` (sends header `x-grok-conv-id: <promptCacheKey>`) |
| **Images (`xai-images`)** | `POST /images/generations` or `POST /images/edits` |
| **Video (`xai-video`)** | `POST /videos/generations`, `POST /videos/edits`, or `POST /videos/extensions` -> Poll `GET /videos/${requestID}` |
| **SuperGrok Device Code Request** | `POST https://auth.x.ai/oauth2/device/code` (`client_id: "b1a00492-073a-47ea-816f-4c329264a828"`, `scope: "openid profile email offline_access grok-cli:access api:access"`, `referrer: "opencode"`) |
| **Device Token Polling / Refresh** | `POST https://auth.x.ai/oauth2/token` |
| **Authentication** | `Authorization: Bearer <accessToken \| apiKey>` (env fallback `XAI_API_KEY`) |

---

## 3. Implementation (`packages/ai/src/providers/xai.ts` & `xai-responses.ts`)

```typescript
const responsesRoute = Route.make({
  compact: { endpoint: XAIResponses.compact },
  id: "xai-responses",
  provider: id,
  providerMetadataKey: "xai",
  protocol: XAIResponses.protocol,
  endpoint: Endpoint.path("/responses", { baseURL }),
  transport: OpenResponsesChannel.transport({
    id: "xai-responses",
    name: "xAI Responses",
    rotateAfterMs: RESPONSES_WEBSOCKET_ROTATE_AFTER_MS,
    continuation: ({ instructions: _instructions, ...request }) => (request.store === false ? undefined : request),
  }),
  defaults: { providerOptions: { store: false, include: ["reasoning.encrypted_content"] } },
})

const chatRoute = Route.make({
  id: "xai-chat",
  provider: id,
  providerMetadataKey: "xai",
  protocol: OpenAIChat.protocol,
  endpoint: Endpoint.path("/chat/completions", { baseURL }),
  framing: OpenAIChat.framing,
  headers: ({ request }): Record<string, string> =>
    request.promptCacheKey ? { "x-grok-conv-id": request.promptCacheKey } : {},
})
```
- Round-trips hosted tool items: `web_search_call`, `x_search_call`, `file_search_call`, `code_interpreter_call`, `image_generation_call`, and `mcp_call`.
