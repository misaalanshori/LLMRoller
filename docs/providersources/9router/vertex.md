# Google Cloud Vertex AI (`vertex`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `vertex`
- **Display Name**: `Vertex AI`
- **Alias**: `vx`
- **Category**: `freeTier` / `apikey` (supports Service Account JSON, Google ADC JSON, OAuth access token, or raw Vertex API key)
- **Service Kinds**: `["llm", "imageToText", "video"]`
- **Executor**: `VertexExecutor` (`open-sse/executors/vertex.js`)
- **Video Adapter**: `open-sse/handlers/videoProviders/vertex.js`
- **Website**: `https://cloud.google.com/vertex-ai`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat (Project-Scoped / OAuth / SA / ADC)** | `POST https://aiplatform.googleapis.com/v1/projects/{projectId}/locations/{location}/publishers/google/models/{model}:{streamGenerateContent?alt=sse\|generateContent}` |
| **Chat (Projectless Raw API Key)** | `POST https://aiplatform.googleapis.com/v1/publishers/google/models/{model}:{streamGenerateContent?alt=sse\|generateContent}&key={rawKey}` |
| **Veo Video Creation** | `POST https://aiplatform.googleapis.com/v1/projects/{projectId}/locations/{location}/publishers/google/models/{model}:predictLongRunning` |
| **Veo Video Polling** | `POST https://aiplatform.googleapis.com/v1/projects/{projectId}/locations/{location}/publishers/google/models/{model}:fetchPredictOperation` |

### Supported Authentication Modes (`open-sse/executors/vertex.js`)
1. **Service Account JSON (`type: "service_account"`)**: Mints a short-lived Google OAuth2 Bearer token via JWT assertion (`refreshVertexToken`) and uses `project_id` from the JSON.
2. **Google ADC User Credential JSON (`type: "authorized_user"`)**: Refreshes token using `client_id`, `client_secret`, and `refresh_token`, using `quota_project_id`.
3. **Raw Vertex API Key**: Appended as `?key={rawKey}` query parameter; if `projectId` is missing and needed, `resolveProjectId(apiKey)` probes `POST .../publishers/google/models/__probe__:generateContent?key=${apiKey}` and parses `projects/{id}/` from the 404 error message.

---

## 3. URL Builder & Request Handling (`open-sse/executors/vertex.js`)

```javascript
buildUrl(model, stream, urlIndex = 0, credentials = null) {
  const saJson = parseVertexSaJson(credentials?.apiKey);
  const adcJson = parseVertexAdcJson(credentials?.apiKey);
  const usesOAuth = !!saJson || !!adcJson || !!credentials?.accessToken;
  const rawKey = !usesOAuth ? credentials?.apiKey : null;
  const projectId =
    saJson?.project_id ||
    adcJson?.quota_project_id ||
    credentials?.providerSpecificData?.projectId;

  const action = stream ? "streamGenerateContent" : "generateContent";

  if (usesOAuth) {
    if (!projectId) {
      throw new Error(
        "Vertex OAuth/ADC requires a project_id. " +
        "Add quota_project_id to your ADC JSON or set providerSpecificData.projectId."
      );
    }
    const location = credentials?.providerSpecificData?.location
      || getModelMeta(this.provider, model)?.defaultLocation
      || "us-central1";
    let url = `https://aiplatform.googleapis.com/v1/projects/${projectId}/locations/${location}/publishers/google/models/${model}:${action}`;
    if (stream) url += "?alt=sse";
    return url;
  }

  const location = credentials?.providerSpecificData?.location || "global";
  let url = projectId
    ? `https://aiplatform.googleapis.com/v1/projects/${projectId}/locations/${location}/publishers/google/models/${model}:${action}`
    : `https://aiplatform.googleapis.com/v1/publishers/google/models/${model}:${action}`;
  if (stream) url += "?alt=sse";
  if (rawKey) url += stream ? `&key=${rawKey}` : `?key=${rawKey}`;
  return url;
}
```

---

## 4. Veo Video Adapter (`open-sse/handlers/videoProviders/vertex.js`)

- Translates OpenAI-style `/v1/videos` creation payload into `{ instances: [{ prompt, image?, video? }], parameters: { sampleCount, durationSeconds, aspectRatio, resolution, seed, negativePrompt, storageUri, generateAudio } }`.
- Base64url-encodes the Vertex operation resource name (`projects/{p}/locations/{l}/publishers/{pub}/models/{m}/operations/{op}`) into a flat `id` so `GET /v1/videos/{id}` polls `POST {modelPath}:fetchPredictOperation` with `{ operationName }`.

---

## 5. Supported Models (`open-sse/providers/registry/vertex.js`)

- **LLM**:
  - `gemini-4-argon` (`Gemini 4 Argon`, `defaultLocation: "global"`)
  - `gemini-3.1-pro-preview` (`Gemini 3.1 Pro Preview`)
  - `gemini-3.1-flash-lite-preview` (`Gemini 3.1 Flash Lite Preview`)
  - `gemini-3-flash-preview` (`Gemini 3 Flash Preview`)
  - `gemini-2.5-flash` (`Gemini 2.5 Flash`)
- **Video**:
  - `veo-3.1-generate-preview` (`Veo 3.1 (Preview)`)
  - `veo-3.1-fast-generate-preview` (`Veo 3.1 Fast (Preview)`)
  - `veo-3.0-generate-001` (`Veo 3`)
  - `veo-2.0-generate-001` (`Veo 2`)
