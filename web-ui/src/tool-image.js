const imageDataUrl = /^data:image\/(?:png|jpeg|webp|gif|avif);base64,[a-z0-9+/=]+$/i;

export function toolOutputImageUrls(value) {
  const images = [];
  const visit = (part, depth) => {
    if (depth > 8 || part == null) return;
    if (typeof part === "string") {
      if (imageDataUrl.test(part)) {
        images.push(part);
      } else if (/^\s*[\[{]/.test(part)) {
        try {
          visit(JSON.parse(part), depth + 1);
        } catch {}
      }
      return;
    }
    if (Array.isArray(part)) {
      part.forEach((item) => visit(item, depth + 1));
      return;
    }
    if (typeof part !== "object") return;
    if (typeof part.image_url === "string" && imageDataUrl.test(part.image_url))
      images.push(part.image_url);
    if (
      part.type === "image" &&
      typeof part.data === "string" &&
      !imageDataUrl.test(part.image_url || "")
    ) {
      const mime = part.mimeType || part.mime_type || "image/png";
      const url = `data:${mime};base64,${part.data}`;
      if (imageDataUrl.test(url)) images.push(url);
    }
    for (const [key, nested] of Object.entries(part)) {
      if (key !== "image_url" && !(key === "data" && part.type === "image"))
        visit(nested, depth + 1);
    }
  };
  visit(value, 0);
  return images;
}

export function toolOutputImageUrl(value) {
  return toolOutputImageUrls(value).at(-1) || null;
}
