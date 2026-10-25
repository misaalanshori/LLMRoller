# GitHub Copilot (`github-copilot`) Provider Specification (`pi`)

## 1. Overview & Identification

- **Provider ID**: `github-copilot`
- **Display Name**: `GitHub Copilot`
- **Provider Module**: `packages/ai/src/providers/github-copilot.ts`
- **OAuth Implementation**: `packages/ai/src/auth/oauth/github-copilot.ts`
- **Dynamic Headers Helper**: `packages/ai/src/api/github-copilot-headers.ts`
- **APIs**: `anthropic-messages`, `openai-completions`, `openai-responses`
- **Models Catalog**: `packages/ai/src/providers/github-copilot.models.ts` (`GITHUB_COPILOT_MODELS`)

---

## 2. Endpoints, OAuth Device Flow & Headers (`packages/ai/src/auth/oauth/github-copilot.ts`)

| Operation | Endpoint |
|---|---|
| **Default Base URL** | `https://api.individual.githubcopilot.com` (or `https://api.<proxy-ep>` parsed from Copilot token `proxy-ep=...`, or `https://copilot-api.<enterpriseDomain>`) |
| **Device Code Request** | `POST https://${domain}/login/device/code` (`client_id: "Iv1.b507a08c87ecfe98"`, `scope: "read:user"`) |
| **Access Token Polling** | `POST https://${domain}/login/oauth/access_token` |
| **Copilot Session Token Exchange** | `GET https://api.${domain}/copilot_internal/v2/token` (`Authorization: token <githubAccessToken>`) |
| **Model Policy Enablement** | `POST ${baseUrl}/models/${modelId}/policy` (`{ state: "enabled" }`, `X-GitHub-Api-Version: 2026-06-01`) |
| **Live Models Catalog** | `GET ${baseUrl}/models` |

### Static & Dynamic Headers
```typescript
const COPILOT_HEADERS = {
	"User-Agent": "GitHubCopilotChat/0.35.0",
	"Editor-Version": "vscode/1.107.0",
	"Editor-Plugin-Version": "copilot-chat/0.35.0",
	"Copilot-Integration-Id": "vscode-chat",
} as const;
```
- **Dynamic Request Headers (`buildCopilotDynamicHeaders`)**:
  - `X-Initiator`: `"agent"` (if last message role is not `"user"`) or `"user"`
  - `Openai-Intent`: `"conversation-edits"`
  - `Copilot-Vision-Request: "true"` (when any user or toolResult message contains an image)

---

## 3. Provider Implementation (`packages/ai/src/providers/github-copilot.ts`)

```typescript
export function githubCopilotProvider(): Provider<"anthropic-messages" | "openai-completions" | "openai-responses"> {
	return createProvider({
		id: "github-copilot",
		name: "GitHub Copilot",
		baseUrl: "https://api.individual.githubcopilot.com",
		auth: {
			apiKey: envApiKeyAuth("GitHub Copilot token", ["COPILOT_GITHUB_TOKEN"]),
			oauth: lazyOAuth({ name: "GitHub Copilot", isSubscription: true, load: loadGitHubCopilotOAuth }),
		},
		models: Object.values(GITHUB_COPILOT_MODELS),
		filterModels: (models, credential) => {
			if (credential?.type !== "oauth") return models;
			const availableModelIds = credential.availableModelIds;
			if (!Array.isArray(availableModelIds) || !availableModelIds.every((id) => typeof id === "string")) {
				return models;
			}
			const available = new Set(availableModelIds);
			return models.filter((model) => available.has(model.id));
		},
		api: {
			"anthropic-messages": anthropicMessagesApi(),
			"openai-completions": openAICompletionsApi(),
			"openai-responses": openAIResponsesApi(),
		},
	});
}
```
