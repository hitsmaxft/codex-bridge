function appendPlain(parent, text) {
  text.split("\n").forEach((part, index) => {
    if (index) parent.appendChild(document.createElement("br"));
    parent.appendChild(document.createTextNode(part));
  });
}

function appendInline(parent, text) {
  const pattern = /(`[^`\n]+`|\*\*[^*\n]+\*\*|\[[^\]\n]+\]\([^\s)]+\))/g;
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
    } else {
      const parts = token.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
      const link = document.createElement("a");
      link.textContent = parts[1];
      try {
        const url = new URL(parts[2], location.href);
        if (["http:", "https:", "mailto:"].includes(url.protocol)) {
          link.href = url.href;
          link.target = "_blank";
          link.rel = "noopener noreferrer";
        }
      } catch {
        // Invalid links remain inert text.
      }
      if (link.href) parent.appendChild(link);
      else parent.appendChild(document.createTextNode(token));
    }
    offset = match.index + token.length;
  }
  appendPlain(parent, text.slice(offset));
}

export function markdownNode(source) {
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
      appendInline(element, heading[2]);
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
        appendInline(item, next[2]);
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
      appendInline(quote, parts.join("\n"));
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
    appendInline(paragraph, parts.join("\n"));
    root.appendChild(paragraph);
  }
  return root;
}
