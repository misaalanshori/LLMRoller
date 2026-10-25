# SiliconFlow (`siliconflow`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `siliconflow`
- **Display Name**: `SiliconFlow`
- **Alias**: `sf`
- **Category**: `apikey`
- **Service Kinds**: `["llm"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`)
- **Website**: `https://siliconflow.cn`

---

## 2. Endpoints & Authentication

| Parameter | Value |
|---|---|
| **Base URL** | `https://api.siliconflow.com/v1/chat/completions` |
| **Models / Validation URL** | `https://api.siliconflow.com/v1/models` |
| **Auth Header** | `Authorization: Bearer <apiKey>` |
| **Wire Format** | `"openai"` (`thinkingFormat: "openai"`) |

---

## 3. Supported Models (`open-sse/config/providerModels.js`)

| Model ID | Display Name |
|---|---|
| `deepseek-ai/DeepSeek-V4-Pro` | DeepSeek V4 Pro |
| `deepseek-ai/DeepSeek-V3.2` | DeepSeek V3.2 |
| `deepseek-ai/DeepSeek-R1` | DeepSeek R1 |
| `deepseek-ai/DeepSeek-V3` | DeepSeek V3 |
| `Qwen/Qwen3.5-397B-A17B` | Qwen3.5 397B A17B |
| `Qwen/Qwen3-235B-A22B-Thinking-2507` | Qwen3 235B A22B Thinking |
| `Qwen/Qwen3-235B-A22B-Instruct-2507` | Qwen3 235B A22B Instruct |
| `Qwen/Qwen3-Coder-480B-A35B-Instruct` | Qwen3 Coder 480B A35B |
| `Qwen/Qwen3-32B` | Qwen3 32B |
| `Pro/zai-org/GLM-5` | GLM 5 Pro |
| `Pro/zai-org/GLM-4.7` | GLM 4.7 Pro |
| `zai-org/GLM-4.6` | GLM 4.6 |
| `zai-org/GLM-4.6V` | GLM 4.6V |
| `Pro/MiniMaxAI/MiniMax-M2.5` | MiniMax M2.5 Pro |
| `Pro/moonshotai/Kimi-K2.5` | Kimi K2.5 Pro |
| `moonshotai/Kimi-K2-Thinking` | Kimi K2 Thinking |
| `bytedance-seed/Seed-OSS-36B-Instruct` | Seed OSS 36B Instruct |
| `stepfun-ai/Step-3.5-Flash` | Step 3.5 Flash |
| `XiaomiMiMo/MiMo-V2-Flash` | MiMo V2 Flash |
| `inclusionAI/Ling-flash-2.0` | Ling Flash 2.0 |
| `inclusionAI/Ring-flash-2.0` | Ring Flash 2.0 |
| `tencent/Hunyuan-A13B-Instruct` | Hunyuan A13B Instruct |
| `Baidu/ERNIE-4.5-300B-A47B` | ERNIE 4.5 300B A47B |
