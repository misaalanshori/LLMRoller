# Azure OpenAI (`azure`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `azure`
- **Alias**: `azure`
- **Display Name**: Azure OpenAI
- **Category**: `apikey`
- **Has Provider Specific Data**: `true`
- **Website**: `https://azure.microsoft.com/en-us/products/ai-services/openai-service`
- **Executor**: `AzureExecutor` (`open-sse/executors/azure.js`, subclass of `DefaultExecutor`)
- **Wire Format**: `openai` (OpenAI Chat Completions compatible)

## 2. Dynamic Endpoint Resolution
`AzureExecutor.buildUrl(model, stream, urlIndex, credentials)` constructs the URL dynamically from connection `providerSpecificData` or environment variables:
- **Base Endpoint (`azureEndpoint`)**:
  1. `credentials.providerSpecificData.azureEndpoint`
  2. `process.env.AZURE_ENDPOINT`
  3. Fallback: `"https://api.openai.com"`
  (Trailing slash stripped)
- **Deployment Name (`deployment`)**:
  1. `credentials.providerSpecificData.deployment`
  2. `model` (request model parameter)
  3. `process.env.AZURE_DEPLOYMENT`
  4. Fallback: `"gpt-4"`
- **API Version (`apiVersion`)**:
  1. `credentials.providerSpecificData.apiVersion`
  2. `process.env.AZURE_API_VERSION`
  3. Fallback: `"2024-10-01-preview"`
- **Constructed URL**:
  ```
  POST {endpoint}/openai/deployments/{deployment}/chat/completions?api-version={apiVersion}
  ```

## 3. Authentication & Headers
- **API Key Header**: Uses `api-key: <apiKey>` (instead of `Authorization: Bearer`), resolved from:
  1. `credentials.apiKey`
  2. `credentials.accessToken`
  3. `process.env.OPENAI_API_KEY`
- **Optional Organization Header**: If `credentials.providerSpecificData.organization` or `process.env.AZURE_ORGANIZATION` is set, sends:
  - `OpenAI-Organization: <organization>`
- **Standard Headers**:
  - `Content-Type: application/json`
  - `Accept: text/event-stream` (when `stream: true`)

## 4. Request Transformation
`AzureExecutor.transformRequest()` returns `body` untouched (bypassing `DefaultExecutor`'s `stripUnsupportedParams` and `injectReasoningContent`).

## 5. Source Implementation

### Registry (`open-sse/providers/registry/azure.js`)
```javascript
export default {
  id: "azure",
  priority: 40,
  alias: "azure",
  display: {
    name: "Azure OpenAI",
    icon: "cloud",
    color: "#0078D4",
    textIcon: "AZ",
    website: "https://azure.microsoft.com/en-us/products/ai-services/openai-service",
    notice: {
      apiKeyUrl: "https://portal.azure.com/#view/Microsoft_Azure_ProjectOxford/CognitiveServicesHub/~/OpenAI",
    },
  },
  category: "apikey",
  hasProviderSpecificData: true,
  transport: {
    baseUrl: "",
    headers: {},
  },
};
```

### Executor (`open-sse/executors/azure.js`)
```javascript
import { DefaultExecutor } from "./default.js";

export class AzureExecutor extends DefaultExecutor {
  constructor() {
    super("azure");
  }

  buildUrl(model, stream, urlIndex = 0, credentials = null) {
    const azureEndpoint = credentials?.providerSpecificData?.azureEndpoint
      || process.env.AZURE_ENDPOINT
      || "https://api.openai.com";

    const apiVersion = credentials?.providerSpecificData?.apiVersion
      || process.env.AZURE_API_VERSION
      || "2024-10-01-preview";

    const deployment = credentials?.providerSpecificData?.deployment
      || model
      || process.env.AZURE_DEPLOYMENT
      || "gpt-4";

    const endpoint = azureEndpoint.replace(/\/$/, "");
    return `${endpoint}/openai/deployments/${deployment}/chat/completions?api-version=${apiVersion}`;
  }

  buildHeaders(credentials, stream = true) {
    const headers = {
      "Content-Type": "application/json",
      ...this.config.headers
    };

    const apiKey = credentials?.apiKey
      || credentials?.accessToken
      || process.env.OPENAI_API_KEY;

    if (apiKey) {
      headers["api-key"] = apiKey;
    }

    const organization = credentials?.providerSpecificData?.organization
      || process.env.AZURE_ORGANIZATION;

    if (organization) {
      headers["OpenAI-Organization"] = organization;
    }

    if (stream) {
      headers["Accept"] = "text/event-stream";
    }

    return headers;
  }

  transformRequest(model, body, stream, credentials) {
    return body;
  }
}
```
