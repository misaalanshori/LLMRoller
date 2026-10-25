# Google Cloud Vertex AI (`google-vertex`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `google-vertex`
- **Package Entrypoints**:
  - `@opencode/ai/providers/google-vertex` (`packages/ai/src/providers/google-vertex.ts`)
  - `@opencode/ai/providers/google-vertex-chat` (`packages/ai/src/providers/google-vertex-chat.ts`)
  - `@opencode/ai/providers/google-vertex-interactions` (`packages/ai/src/providers/google-vertex-interactions.ts`)
  - `@opencode/ai/providers/google-vertex-messages` (`packages/ai/src/providers/google-vertex-messages.ts`)
  - `@opencode/ai/providers/google-vertex-mistral` (`packages/ai/src/providers/google-vertex-mistral.ts`)
  - `@opencode/ai/providers/google-vertex-responses` (`packages/ai/src/providers/google-vertex-responses.ts`)
  - `packages/ai/src/providers/google-vertex-shared.ts`
- **Core Plugin**: `packages/core/src/plugin/provider/google-vertex.ts` (`opencode.provider.google.vertex`)

---

## 2. Regional Host Mapping, Routes & Authentication (`google-vertex-shared.ts`)

### 2.1 Host Resolution (`GoogleVertexShared.host(location)`)
- `location === "global"` -> `aiplatform.googleapis.com`
- `location === "eu" || location === "us"` -> `aiplatform.${location}.rep.googleapis.com` (Jurisdictional Regional Endpoint Platform)
- Other regional locations -> `${location}-aiplatform.googleapis.com`

### 2.2 Multi-Protocol Routes

| Route | Base URL & Path | Auth |
|---|---|---|
| **Gemini (`google-vertex-gemini`)** | **ADC/OAuth**: `https://${host}/v1beta1/projects/${project}/locations/${location}/publishers/google/models/${model}:streamGenerateContent?alt=sse` (or `/endpoints/${id}:streamGenerateContent?alt=sse`)<br>**Express Mode API Key**: `https://aiplatform.googleapis.com/v1/publishers/google/models/${model}:streamGenerateContent?alt=sse` | `Authorization: Bearer <token>` or `x-goog-api-key: <apiKey>` |
| **Interactions (`google-vertex-interactions`)** | `https://${host}/v1beta1/[projects/${project}/]locations/${location}/interactions?alt=sse` | `Authorization: Bearer <token>` or `x-goog-api-key: <apiKey>` |
| **Anthropic Messages (`google-vertex-messages`)** | `https://${host}/v1/projects/${project}/locations/${location}/publishers/anthropic/models/${model}:streamRawPredict` (injects `anthropic_version: "vertex-2023-10-16"` in body, omits `model` from body, sets header `anthropic-version: 2023-06-01`) | `Authorization: Bearer <token>` (OAuth/ADC only) |
| **Mistral (`google-vertex-mistral`)** | `https://${host}/v1/projects/${project}/locations/${location}/publishers/mistralai/models/${model}:streamRawPredict` (omits `prompt_cache_key` from body) | `Authorization: Bearer <token>` (OAuth/ADC only) |
| **OpenAI Chat (`google-vertex-chat`)** | `https://aiplatform.googleapis.com/v1/projects/${project}/locations/${location}/endpoints/openapi/chat/completions` | `Authorization: Bearer <token>` (OAuth/ADC only) |
| **OpenAI Responses (`google-vertex-responses`)** | `https://aiplatform.googleapis.com/v1/projects/${project}/locations/${location}/endpoints/openapi/responses` | `Authorization: Bearer <token>` (OAuth/ADC only) |

- **Service Tier Header**: On Gemini and Interactions routes, `providerOptions.serviceTier` is moved from the body to the header `x-vertex-ai-llm-shared-request-type: <serviceTier>`.
- **ADC Discovery (`CloudSDKAuth`)**: Subclasses `google-auth-library`'s `GoogleAuth` to honor `CLOUDSDK_CONFIG` when locating `application_default_credentials.json`, with scope `https://www.googleapis.com/auth/cloud-platform`.
