function appendPlain(parent, text) {
  text.split("\n").forEach((part, index) => {
    if (index) parent.appendChild(document.createElement("br"));
    parent.appendChild(document.createTextNode(part));
  });
}

export function localFilePath(raw) {
  if (typeof raw !== "string") return null;
  let value = raw.trim();
  if (/^https?:\/\//i.test(value)) {
    try {
      const pathname = new URL(value).pathname;
      // Some model output uses an autolink-shaped URL whose path is an
      // encoded Markdown destination: https://host/%3C/absolute/path%3E.
      // Recover only the wrapped absolute path; the server still enforces the
      // selected session workspace and file-size boundary.
      if (/^\/%3c(?:%2f|\/).*%3e$/i.test(pathname)) value = pathname.slice(1);
      else return null;
    } catch {
      return null;
    }
  }
  try {
    value = decodeURIComponent(value);
  } catch {
    // A literal percent sign is valid in a local filename. Keep the original
    // value when the Markdown target is not valid percent-encoded text.
  }
  if (value.startsWith("<") && value.endsWith(">")) {
    value = value.slice(1, -1).trim();
  }
  let path = null;
  if (value.startsWith("/")) path = value;
  else if (value.startsWith("file://")) {
    try {
      path = decodeURIComponent(new URL(value).pathname);
    } catch {
      return null;
    }
  }
  return path;
}

function appendLink(parent, label, target, options) {
  const link = document.createElement("a"),
    downloadPath = localFilePath(target);
  link.textContent = label;
  if (downloadPath && options.requestLocalFileDownload) {
    link.href = "#";
    link.download = downloadPath.split("/").at(-1) || "download";
    link.onclick = async (event) => {
      event.preventDefault();
      if (link.getAttribute("aria-busy") === "true") return;
      link.setAttribute("aria-busy", "true");
      try {
        const url = await options.requestLocalFileDownload(downloadPath);
        // Location navigation is more reliable than a detached synthetic
        // anchor in iOS browsers and still preserves the server-provided
        // Content-Disposition filename.
        window.location.assign(url);
      } catch (error) {
        options.onError?.(error);
      } finally {
        link.removeAttribute("aria-busy");
      }
    };
  } else {
    try {
      const url = new URL(target, location.href);
      if (["http:", "https:", "mailto:"].includes(url.protocol)) {
        link.href = url.href;
        link.target = "_blank";
        link.rel = "noopener noreferrer";
      }
    } catch {
      // Invalid links remain inert text.
    }
  }
  if (link.href) parent.appendChild(link);
  else parent.appendChild(document.createTextNode(label));
}

function appendInline(parent, text, options) {
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\[[^\]\n]+\]\([^\s)]+\)|https?:\/\/[^\s<>()]+)/g;
  let offset = 0;
  for (const match of text.matchAll(pattern)) {
    appendPlain(parent, text.slice(offset, match.index));
    const token = match[0];
    if (token.startsWith("`")) {
      const code = document.createElement("code");
      code.textContent = token.slice(1, -1);
      parent.appendChild(code);
    } else if (token.startsWith("**")) {
      const strong = document.createElement("strong");
      strong.textContent = token.slice(2, -2);
      parent.appendChild(strong);
    } else if (token.startsWith("[")) {
      const parts = token.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
      appendLink(parent, parts[1], parts[2], options);
    } else appendLink(parent, token, token, options);
    offset = match.index + token.length;
  }
  appendPlain(parent, text.slice(offset));
}

export function markdownNode(source, options = {}) {
  const root = document.createElement("div");
  root.className = "markdown";
  const lines = String(source || "")
    .replace(/\r\n?/g, "\n")
    .split("\n");
  let index = 0;
  const blockStart = (line) =>
    /^\s*```|^#{1,6}\s+|^\s*(?:[-*+] |\d+[.)] )|^\s*>\s?|^\s*([-*_])(?:\s*\1){2,}\s*$/.test(line);

  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }
    const fence = line.match(/^\s*```\s*([\w-]*)\s*$/);
    if (fence) {
      const pre = document.createElement("pre");
      const code = document.createElement("code");
      if (fence[1]) code.className = `language-${fence[1]}`;
      const body = [];
      for (index += 1; index < lines.length && !/^\s*```\s*$/.test(lines[index]); index += 1) {
        body.push(lines[index]);
      }
      if (index < lines.length) index += 1;
      code.textContent = body.join("\n");
      pre.appendChild(code);
      root.appendChild(pre);
      continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.+)$/);
    if (heading) {
      const element = document.createElement(`h${heading[1].length}`);
      appendInline(element, heading[2], options);
      root.appendChild(element);
      index += 1;
      continue;
    }
    if (/^\s*([-*_])(?:\s*\1){2,}\s*$/.test(line)) {
      root.appendChild(document.createElement("hr"));
      index += 1;
      continue;
    }
    const listItem = line.match(/^\s*([-*+] |\d+[.)] )(.+)$/);
    if (listItem) {
      const ordered = /^\d/.test(listItem[1]);
      const list = document.createElement(ordered ? "ol" : "ul");
      while (index < lines.length) {
        const next = lines[index].match(/^\s*([-*+] |\d+[.)] )(.+)$/);
        if (!next || /^\d/.test(next[1]) !== ordered) break;
        const item = document.createElement("li");
        appendInline(item, next[2], options);
        list.appendChild(item);
        index += 1;
      }
      root.appendChild(list);
      continue;
    }
    if (/^\s*>/.test(line)) {
      const quote = document.createElement("blockquote");
      const parts = [];
      while (index < lines.length && /^\s*>/.test(lines[index])) {
        parts.push(lines[index].replace(/^\s*>\s?/, ""));
        index += 1;
      }
      appendInline(quote, parts.join("\n"), options);
      root.appendChild(quote);
      continue;
    }
    const parts = [line];
    for (
      index += 1;
      index < lines.length && lines[index].trim() && !blockStart(lines[index]);
      index += 1
    ) {
      parts.push(lines[index]);
    }
    const paragraph = document.createElement("p");
    appendInline(paragraph, parts.join("\n"), options);
    root.appendChild(paragraph);
  }
  return root;
}
