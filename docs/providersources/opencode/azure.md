# Azure OpenAI & AI Foundry (`azure`) Provider Specification (`opencode`)

## 1. Overview & Identification

- **Provider ID**: `azure`
- **Package Entrypoints**:
  - `@opencode/ai/providers/azure` (`packages/ai/src/providers/azure.ts`)
  - `@opencode/ai/providers/azure/chat` (`packages/ai/src/providers/azure/chat.ts`)
  - `@opencode/ai/providers/azure/responses` (`packages/ai/src/providers/azure/responses.ts`)
- **Core Plugin**: `packages/core/src/plugin/provider/azure.ts` (`opencode.provider.azure`)

---

## 2. Endpoints, Transports & Authentication

| Parameter | Value |
|---|---|
| **Resource Base URL** | `https://${resourceName}.openai.azure.com/openai` |
| **Standard v1 Path (`useDeploymentBasedUrls: false`)** | `${baseURL}/v1/responses?api-version=v1` or `${baseURL}/v1/chat/completions?api-version=v1` |
| **Deployment-Based Path (`useDeploymentBasedUrls: true`)** | `${baseURL}/deployments/${modelID}/responses?api-version=${apiVersion}` or `/chat/completions` |
| **WebSocket Transport (`azure-openai-responses`)** | Enabled when `https://*.openai.azure.com/openai/v1/responses?api-version=v1` (strips `api-version` on WS URL, rotates after 55 minutes) |
| **API Key Auth** | `api-key: <key>` (explicitly removes `authorization` header; env fallback `AZURE_OPENAI_API_KEY`, `AZURE_API_KEY`, `AZURE_COGNITIVE_SERVICES_API_KEY`) |
| **Entra ID Auth (`azure-cli` external method)** | Executes `az account get-access-token --scope <scope> --output json` (`https://cognitiveservices.azure.com/.default` or `https://ai.azure.com/.default` for `*.services.ai.azure.com`), strips `api-key`/`x-api-key`, and injects `authorization: Bearer <token>` on both HTTP and WebSocket handshakes |

---

## 3. Provider Package Implementation (`packages/ai/src/providers/azure.ts`)

```typescript
const resourceBaseURL = (resourceName: string) => `https://${resourceName.trim()}.openai.azure.com/openai`

function endpoint(input: Config, modelID: string | ModelID) {
  const baseURL = Endpoint.trimBaseUrl(input.baseURL ?? resourceBaseURL(input.resourceName!))
  const query = { "api-version": input.apiVersion ?? "v1", ...input.queryParams }

  if (input.useDeploymentBasedUrls) return { baseURL: `${baseURL}/deployments/${modelID}`, query }
  if (input.baseURL !== undefined && !new URL(input.baseURL).hostname.endsWith(".openai.azure.com")) {
    return { baseURL, query: input.queryParams }
  }
  return { baseURL: `${baseURL}/v1`, query }
}
```

---

## 4. Core Plugin Deployment Discovery (`packages/core/src/plugin/provider/azure.ts`)

- **Azure Resource Graph Lookup** (for `azure-cli` credentials):
  `POST https://management.azure.com/providers/Microsoft.ResourceGraph/resources?api-version=2022-10-01` with Kusto query matching `properties.customSubDomainName =~ '${resource}'`, followed by paginated `GET https://management.azure.com${id}/deployments?api-version=2024-10-01`.
- **Data-Plane Fallback / API Key Discovery**:
  `GET https://${name}.openai.azure.com/openai/deployments?api-version=2022-12-01` (`status === "succeeded"`).
