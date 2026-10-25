# Together AI (`together`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `together`
- **Display Name**: `Together AI`
- **Category**: `apikey`
- **Service Kinds**: `["llm", "embedding", "image"]`
- **Executor**: `DefaultExecutor` (`open-sse/executors/default.js`) + `openaiImage.js`
- **Website**: `https://together.ai`

---

## 2. Endpoints & Authentication

| Service | Endpoint |
|---|---|
| **Chat Completions** | `POST https://api.together.xyz/v1/chat/completions` |
| **Embeddings** | `POST https://api.together.xyz/v1/embeddings` |
| **Image Generation** | `POST https://api.together.xyz/v1/images/generations` |
| **Models / Validation URL** | `GET https://api.together.xyz/v1/models` |
| **Authentication** | `Authorization: Bearer <apiKey>` |

---

## 3. Configuration & Supported Models

### 3.1 LLM Models (`open-sse/config/providerModels.js`)
- `meta-llama/Llama-3.3-70B-Instruct-Turbo` (`Llama 3.3 70B Turbo`)
- `deepseek-ai/DeepSeek-R1` (`DeepSeek R1`)
- `Qwen/Qwen3-235B-A22B` (`Qwen3 235B`)
- `meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8` (`Llama 4 Maverick`)

### 3.2 Embedding Models (`open-sse/config/embeddingRegistry.js`)
- `BAAI/bge-large-en-v1.5` (`BGE Large EN v1.5`, `dimensions: 1024`)
- `togethercomputer/m2-bert-80M-8k-retrieval` (`M2 BERT 80M 8K`, `dimensions: 768`)

### 3.3 Image Models (`open-sse/config/imageRegistry.js`)
- `black-forest-labs/FLUX.1-schnell-Free` (`FLUX.1 Schnell (Free)`)
- `black-forest-labs/FLUX.1-schnell` (`FLUX.1 Schnell`)
- `black-forest-labs/FLUX.1-dev` (`FLUX.1 Dev`)
- `black-forest-labs/FLUX.1.1-pro` (`FLUX.1.1 Pro`)
