# Cloudflare Workers AI (`cloudflare-workers-ai`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `cloudflare-workers-ai`
- **Package Entrypoint**: `@opencode/ai/providers/cloudflare-workers-ai` (`packages/ai/src/providers/cloudflare-workers-ai.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/cloudflare-workers-ai.ts` (`opencode.provider.cloudflare.workers.ai`)
- **Protocols**:
  - `OpenAIChat.protocol` (`cloudflare-workers-ai`)
  - `CloudflareWorkersAIImages.protocol` (`packages/ai/src/protocols/cloudflare-workers-ai-images.ts`)
  - `CloudflareRunEvaluation.model` (`packages/ai/src/experimental/cloudflare-run-evaluation.ts`)

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat Completions** | `POST https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/v1/chat/completions` |
| **Image Generation (Run API)** | `POST https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/run/${modelID}` |
| **Experimental Run Evaluation** | `POST https://api.cloudflare.com/client/v4/accounts/${accountId}/ai/run/${modelID}` |
| **Authentication** | `Authorization: Bearer <token>` (env fallbacks: `CLOUDFLARE_API_KEY`, `CLOUDFLARE_WORKERS_AI_TOKEN`, `CLOUDFLARE_API_TOKEN`, plus `CLOUDFLARE_ACCOUNT_ID`) |

---

## 3. Image Protocol Quirks (`packages/ai/src/protocols/cloudflare-workers-ai-images.ts`)

- **FLUX.2 Multipart Requirement**: Models starting with `@cf/black-forest-labs/flux-2-` require `multipart/form-data` (`FormData`) even when sending only a text prompt; all other models take `application/json`.
- **Dual Response Decoding**: FLUX and Lucid Origin return JSON `{ result: { image: "<base64>", usage? } }`, whereas Stable Diffusion, DreamShaper, and Phoenix return raw binary image bytes (`!response.headers["content-type"]?.startsWith("application/json")`).

```typescript
const fromRequest = Effect.fn("CloudflareWorkersAIImages.fromRequest")(function* (request: Request) {
  if (request.n !== undefined && request.n > 1)
    return yield* route.unsupported("media.n", `${route.name} generates one image per request; call it once per image`)
  const fields = {
    prompt: request.prompt,
    ...(yield* ImageSizing.exact(route, request, { multiple: 16, minEdge: 256, maxEdge: 2048 })),
  }
  const native = mergeJsonRecords(request.providerOptions, request.http?.body)
  if (!request.model.id.startsWith(MULTIPART_MODEL_PREFIX))
    return MediaProtocol.json(mergeJsonRecords(fields, native) ?? {})
  const body = new FormData()
  MediaInput.appendFields(body, fields, { overlay: native, reserved: RESERVED_FORM_FIELDS })
  return MediaProtocol.multipart(body)
})
```
