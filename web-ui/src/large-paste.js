export const LARGE_PASTE_CHARS = 5000;
export const MAX_PASTED_TEXT_BYTES = 2 * 1024 * 1024;

export function shouldAttachPastedText(text) {
  return typeof text === "string" && text.length >= LARGE_PASTE_CHARS;
}
