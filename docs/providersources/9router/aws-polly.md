# AWS Polly (`aws-polly`) — Upstream Provider Specification (`9router`)

## 1. Overview & Identity
- **Provider ID**: `aws-polly`
- **Alias**: `polly`
- **Display Name**: AWS Polly
- **Category**: `apikey`
- **Authentication Type**: `apikey`
- **Service Kinds**: `["tts"]` (Text-to-Speech)
- **Website**: `https://aws.amazon.com/polly/`
- **API Key URL**: `https://console.aws.amazon.com/iam/home#/security_credentials`
- **Provider-Specific Data**: `hasProviderSpecificData: true` (expects AWS Secret Access Key as `apiKey`, plus `providerSpecificData.accessKeyId` and optional `providerSpecificData.region`).

## 2. Endpoints & Configuration
- **TTS Endpoint Pattern**: `https://polly.{region}.amazonaws.com/v1/speech`
- **TTS Config**:
  - `authType: "apikey"`
  - `authHeader: "aws-sigv4"`
  - `format: "aws-polly"`
- **Implementation Status in `9router`**: Registered in `open-sse/providers/registry/aws-polly.js` with `ttsConfig.format: "aws-polly"`. Note that `open-sse/handlers/ttsProviders/index.js` and `genericFormats.js` do not currently register an `"aws-polly"` entry in `SPECIAL_ADAPTERS` or `FORMAT_HANDLERS`, so `synthesizeViaConfig()` returns `null` (resulting in HTTP 400 `"Provider 'aws-polly' does not support TTS via this route."`) unless a custom handler is added.

## 3. Supported Models / Engines
- `standard` ("Standard")
- `neural` ("Neural")
- `long-form` ("Long-form")
- `generative` ("Generative")

## 4. Source Implementation (`open-sse/providers/registry/aws-polly.js`)
```javascript
export default {
  id: "aws-polly",
  alias: "polly",
  display: {
    name: "AWS Polly",
    icon: "record_voice_over",
    color: "#FF9900",
    textIcon: "PL",
    website: "https://aws.amazon.com/polly/",
    notice: {
      text: "Use AWS Secret Access Key as API key; set providerSpecificData.accessKeyId and optional region.",
      apiKeyUrl: "https://console.aws.amazon.com/iam/home#/security_credentials"
    }
  },
  category: "apikey",
  authType: "apikey",
  serviceKinds: [
    "tts"
  ],
  ttsConfig: {
    baseUrl: "https://polly.{region}.amazonaws.com/v1/speech",
    authType: "apikey",
    authHeader: "aws-sigv4",
    format: "aws-polly",
    models: [
      {
        id: "standard",
        name: "Standard"
      },
      {
        id: "neural",
        name: "Neural"
      },
      {
        id: "long-form",
        name: "Long-form"
      },
      {
        id: "generative",
        name: "Generative"
      }
    ]
  },
  hasProviderSpecificData: true
};
```
