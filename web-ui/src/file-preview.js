import { EditorState } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";

export function createFilePreviewController({
  root,
  title,
  meta,
  body,
  closeButton,
  downloadButton,
  modeButton,
  translate,
  renderMarkdown,
  inspectImage,
}) {
  let editor = null;
  let current = null;
  let currentPosition = null;
  let markdownSource = false;

  const destroyEditor = () => {
    editor?.destroy();
    editor = null;
  };
  const renderText = (content) => {
    destroyEditor();
    const mount = document.createElement("div");
    mount.className = "file-preview-editor";
    body.replaceChildren(mount);
    const extensions = [
        lineNumbers(),
        EditorState.readOnly.of(true),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({
          "aria-label": translate("filePreviewText"),
          spellcheck: "false",
        }),
      ],
      editorState = EditorState.create({ doc: content, extensions });
    let scrollTo;
    if (currentPosition?.line) {
      const line = editorState.doc.line(
          Math.min(Math.max(1, currentPosition.line), editorState.doc.lines),
        ),
        column = Math.min(Math.max(0, (currentPosition.column || 1) - 1), line.length),
        anchor = line.from + column;
      scrollTo = EditorView.scrollIntoView(anchor, { y: "center" });
    }
    editor = new EditorView({
      parent: mount,
      state: editorState,
      scrollTo,
    });
  };
  const renderCurrent = () => {
    if (!current) return;
    modeButton.hidden = current.kind !== "markdown";
    modeButton.textContent = translate(markdownSource ? "renderMarkdown" : "showMarkdownSource");
    if (current.kind === "markdown" && !markdownSource) {
      destroyEditor();
      const rendered = renderMarkdown(current.content || "");
      rendered.classList.add("file-preview-markdown");
      body.replaceChildren(rendered);
      return;
    }
    if (current.kind === "text" || current.kind === "markdown") {
      renderText(current.content || "");
      return;
    }
    destroyEditor();
    if (current.kind === "image" && current.preview_url) {
      const image = document.createElement("img");
      image.className = "file-preview-image";
      image.src = current.preview_url;
      image.alt = current.name;
      image.onclick = () => inspectImage(image.src, image.alt, image);
      body.replaceChildren(image);
      return;
    }
    const unsupported = document.createElement("div");
    unsupported.className = "file-preview-unsupported";
    unsupported.textContent = translate("filePreviewUnsupported");
    body.replaceChildren(unsupported);
  };
  const close = () => {
    destroyEditor();
    current = null;
    currentPosition = null;
    root.hidden = true;
    document.body.classList.remove("file-preview-open");
  };
  const open = (preview, position = null) => {
    current = preview;
    currentPosition = position;
    markdownSource = false;
    title.textContent = preview.name;
    meta.textContent = `${preview.mime_type} · ${formatBytes(preview.size)}`;
    root.hidden = false;
    document.body.classList.add("file-preview-open");
    renderCurrent();
    closeButton.focus();
  };

  closeButton.onclick = close;
  root.onclick = (event) => {
    if (event.target === root) close();
  };
  modeButton.onclick = () => {
    markdownSource = !markdownSource;
    renderCurrent();
  };
  downloadButton.onclick = () => {
    if (current?.download_url) window.location.assign(current.download_url);
  };

  return { open, close, isOpen: () => !root.hidden };
}

function formatBytes(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
}
