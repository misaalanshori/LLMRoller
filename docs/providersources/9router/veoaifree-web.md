# Veo AI Free Web (`veoaifree-web`) Provider Specification (`9router`)

## 1. Overview & Identification

- **Provider ID**: `veoaifree-web`
- **Display Name**: `Veo AI Free Web`
- **Alias**: `veo-free`
- **Category**: `apikey` (`authType: "none"`)
- **Service Kinds**: `["image", "video"]`
- **Executor**: `VeoAiFreeWebExecutor` (`open-sse/executors/veoaifree-web.js`)
- **Image Handler**: `open-sse/handlers/imageProviders/veoaifree-web.js`
- **Website**: `https://veoaifree.com`

---

## 2. Endpoints & WordPress AJAX Protocol

| Step | Endpoint |
|---|---|
| **CSRF Nonce Extraction** | `GET https://veoaifree.com` (regex `/nonce":"([a-f0-9]+)"/`, preserves `Set-Cookie` jar) |
| **WordPress AJAX Endpoint** | `POST https://veoaifree.com/wp-admin/admin-ajax.php` (`Content-Type: application/x-www-form-urlencoded`, `action=veo_video_generator`, `nonce=<nonce>`) |
| **Image Generation Action** | `actionType=banan-image-generator`, `promptIMG=<prompt>`, `totalVariationsIMG=1`, `aspectRatioIMG=IMAGE_ASPECT_RATIO_{LANDSCAPE\|PORTRAIT\|SQUARE}` |
| **Video Submit Action** | `actionType=full-video-generate`, `prompt=<prompt>`, `totalVariations=1`, `aspectRatio=VIDEO_ASPECT_RATIO_{LANDSCAPE\|PORTRAIT\|SQUARE}` -> returns `sceneData` |
| **Video Poll Action** | `actionType=final-video-results`, `sceneData=<sceneData>` (polls every 20s up to 30 times) |

---

## 3. Implementation (`open-sse/executors/veoaifree-web.js`)

```javascript
async generateMedia({ prompt, kind = "video", size, credentials, signal, log }) {
  if (kind !== "video" && kind !== "image") throw new Error("veoaifree-web supports image and video generation only");
  if (!prompt?.trim()) throw new Error("prompt is required");
  const context = buildContext(signal);
  const nonce = await fetchNonce(context);

  if (kind === "image") {
    const result = await postAjax(context, nonce, {
      promptIMG: prompt,
      totalVariationsIMG: "1",
      aspectRatioIMG: mapAspectRatio(size, "IMAGE_ASPECT_RATIO"),
      actionType: "banan-image-generator",
    });
    const trimmed = result.trim();
    if (!trimmed || trimmed === "0" || trimmed.toLowerCase().includes("error")) {
      throw new Error(`veoaifree image generation failed: ${trimmed.slice(0, 120)}`);
    }
    return trimmed.split(",").map((part) => part.trim()).filter(Boolean).map((part) => (
      /^https?:\/\//i.test(part) ? { url: part } : { b64: part, mime: "image/png" }
    ));
  }

  const genResult = await postAjax(context, nonce, {
    prompt,
    totalVariations: "1",
    aspectRatio: mapAspectRatio(size, "VIDEO_ASPECT_RATIO"),
    actionType: "full-video-generate",
  });
  const sceneData = genResult.trim();
  if (!sceneData || sceneData === "0" || sceneData.toLowerCase().includes("error")) {
    throw new Error(`veoaifree video submit failed: ${sceneData.slice(0, 120)}`);
  }

  for (let poll = 0; poll < MAX_POLLS; poll++) {
    if (signal?.aborted) throw new Error("aborted");
    await new Promise((resolve) => setTimeout(resolve, POLL_INTERVAL_MS));
    const pollResult = (await postAjax(context, nonce, { sceneData, actionType: "final-video-results" })).trim();
    if (!pollResult || pollResult === "0" || pollResult.toLowerCase().includes("error")) continue;
    const url = pollResult.split(/[,\n]/).map((part) => part.trim()).find((part) => /^https?:\/\//i.test(part));
    if (!url) continue;
    const buffer = await downloadArtifactWithRetry(context, url);
    return [{ b64: buffer.toString("base64"), mime: "video/mp4" }];
  }
  throw new Error("veoaifree video generation timed out after 10 minutes");
}
```

### Supported Models (`open-sse/providers/registry/veoaifree-web.js`)
- `veo` (`VEO 3.1 (Free)`, `kind: "video"`)
- `seedance` (`Seedance (Free)`, `kind: "video"`)
- `nano-banana` (`Nano Banana (Free)`, `kind: "image"`)
