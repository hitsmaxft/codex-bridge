import "./styles.css";
import { $, command, notify, run, timeText } from "./api.js";
import { applyLanguage, getLanguage, LANGUAGE_STORAGE_KEY, t as tr } from "./i18n.js";
import { markdownNode } from "./markdown.js";
import {
  DRAFT_STORAGE_KEY,
  THEME_STORAGE_KEY,
  applyTheme,
  persistDrafts,
  saveDraft,
  state,
} from "./state.js";
async function loadStatus() {
  const r = await command({ command: "status" }, false),
    backend = r.write_backend || {};
  state.directAppServer = Boolean(backend.app_server_available);
  state.appServerMode = backend.app_server_mode || null;
  const appServer = state.directAppServer
    ? tr("directOnline")
    : state.appServerMode === "desktop_bundled_only"
      ? tr("bundledPrivate")
      : tr("directOffline");
  $("bridgeState").textContent = tr("bridgeReady", {
    protocol: r.protocol_version,
    backend: appServer,
  });
}
async function toggleLanguage() {
  applyLanguage(getLanguage() === "en" ? "zh" : "en");
  renderProjects();
  renderPending();
  showActivity();
  await loadStatus();
  if (state.current) await openThread(state.current, { quiet: true });
}
async function loadProjects() {
  const r = await command({ command: "projects", include_archived: $("archived").checked }, false);
  state.projects = r.projects || [];
  state.projectThreads.clear();
  state.expanded.clear();
  renderProjects();
  if (!state.current && state.projects.length) await toggleProject(state.projects[0], true);
}
async function loadProjectThreads(project, offset = 0) {
  const r = await command(
    {
      command: "project_threads",
      project_path: project.path,
      include_archived: $("archived").checked,
      offset,
      limit: 50,
    },
    false,
  );
  const prior = offset ? state.projectThreads.get(project.path)?.threads || [] : [];
  state.projectThreads.set(project.path, {
    threads: prior.concat(r.threads || []),
    available: r.available || 0,
  });
  renderProjects();
  return state.projectThreads.get(project.path).threads;
}
async function toggleProject(project, autoOpen = false) {
  if (state.expanded.has(project.path) && !autoOpen) {
    state.expanded.delete(project.path);
    renderProjects();
    return;
  }
  state.expanded.add(project.path);
  let threads = state.projectThreads.get(project.path)?.threads;
  if (!threads) threads = await loadProjectThreads(project);
  else renderProjects();
  if (autoOpen && !state.current && threads.length) await openThread(threads[0]);
}
function renderProjects() {
  const q = $("search").value.trim().toLowerCase(),
    root = $("projects");
  root.textContent = "";
  const projects = state.projects.filter(
    (p) =>
      `${p.name} ${p.path}`.toLowerCase().includes(q) ||
      state.projectThreads
        .get(p.path)
        ?.threads.some((t) => `${t.title || ""} ${t.id}`.toLowerCase().includes(q)),
  );
  if (!projects.length) {
    root.innerHTML = `<div class="empty" style="padding:12px">${tr("noMatchingProjects")}</div>`;
    return;
  }
  for (const p of projects) {
    const wrap = document.createElement("section");
    wrap.className = "project";
    const head = document.createElement("div");
    head.className = "project-head";
    const button = document.createElement("button");
    button.className = "project-button";
    button.innerHTML =
      '<span class="chevron"></span><span class="project-copy"><span class="project-name"></span><span class="project-path"></span></span><span class="project-count"></span>';
    button.children[0].textContent = state.expanded.has(p.path) ? "▾" : "▸";
    button.children[1].children[0].textContent = p.name;
    button.children[1].children[1].textContent = p.path;
    button.children[2].textContent = `${p.thread_count}`;
    button.onclick = () => run(() => toggleProject(p));
    const add = document.createElement("button");
    add.className = "project-add";
    add.textContent = "+";
    add.title = tr("createInProject", { project: p.name });
    add.setAttribute("aria-label", add.title);
    add.onclick = () => showCreateDialog(p);
    head.append(button, add);
    wrap.appendChild(head);
    const list = document.createElement("div");
    list.className = "project-threads";
    list.hidden = !state.expanded.has(p.path);
    const data = state.projectThreads.get(p.path);
    if (!data) {
      list.innerHTML = `<div class="empty" style="padding:8px">${tr("openToLoad")}</div>`;
    } else {
      for (const t of data.threads) {
        const b = document.createElement("button");
        b.className = "thread" + (state.current?.id === t.id ? " active" : "");
        b.innerHTML = '<div class="thread-name"></div><div class="thread-meta"></div>';
        b.children[0].textContent = t.title || t.id;
        b.children[1].textContent = `${t.git_branch || tr("noBranch")} · ${timeText(t.updated_at_ms, getLanguage() === "zh" ? "zh-CN" : "en")}${t.archived ? ` · ${tr("archived")}` : ""}`;
        b.onclick = () => openThread(t);
        list.appendChild(b);
      }
      if (data.threads.length < data.available) {
        const more = document.createElement("button");
        more.className = "thread";
        more.textContent = tr("loadMoreSessions", {
          count: data.available - data.threads.length,
        });
        more.onclick = () => run(() => loadProjectThreads(p, data.threads.length));
        list.appendChild(more);
      }
    }
    wrap.appendChild(list);
    root.appendChild(wrap);
  }
}
function showCreateDialog(project) {
  state.creatingProject = project;
  $("createProject").textContent = project.path;
  $("createProgress").textContent = "";
  $("createDialog").hidden = false;
  $("createCurrentBtn").focus();
}
function closeCreateDialog() {
  if ($("createCurrentBtn").disabled) return;
  $("createDialog").hidden = true;
  state.creatingProject = null;
}
async function createThread(worktree) {
  const project = state.creatingProject;
  if (!project) throw new Error(tr("chooseProject"));
  const buttons = [$("createCurrentBtn"), $("createWorktreeBtn"), $("createCancelBtn")];
  buttons.forEach((button) => (button.disabled = true));
  $("createProgress").textContent = worktree ? tr("creatingWorktree") : tr("creatingSession");
  try {
    const r = await command(
      { command: "thread_create", project_path: project.path, worktree, model: null },
      false,
    );
    $("createDialog").hidden = true;
    state.creatingProject = null;
    await loadProjects();
    const targetPath = r.worktree_path || r.project_path,
      target = state.projects.find((item) => item.path === targetPath);
    if (target) {
      state.expanded.add(target.path);
      await loadProjectThreads(target);
    }
    const thread = target
      ? state.projectThreads.get(target.path)?.threads.find((item) => item.id === r.thread.id) ||
        r.thread
      : r.thread;
    await openThread(thread);
    notify(worktree ? tr("createdWorktree") : tr("createdCurrent"));
  } finally {
    buttons.forEach((button) => (button.disabled = false));
    $("createProgress").textContent = "";
  }
}
function byteText(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1048576).toFixed(1)} MB`;
}
function appendContextValue(details, value, label) {
  const values = Array.isArray(value) ? value : [value];
  let rendered = false;
  for (const part of values) {
    if (part?.type === "input_image" && typeof part.image_url === "string") {
      const img = document.createElement("img");
      img.src = part.image_url;
      img.alt = label || tr("attachment");
      details.appendChild(img);
      rendered = true;
      continue;
    }
    if (part?.type === "input_audio" && typeof part.audio_url === "string") {
      const audio = document.createElement("audio");
      audio.controls = true;
      audio.src = part.audio_url;
      details.appendChild(audio);
      rendered = true;
      continue;
    }
    const raw =
      typeof part?.text === "string"
        ? part.text
        : typeof part?.output_text === "string"
          ? part.output_text
          : null;
    if (raw !== null && !/^<\/?image(?:\s|>)/.test(raw.trim())) {
      const pre = document.createElement("pre");
      pre.textContent = raw;
      details.appendChild(pre);
      rendered = true;
    }
  }
  if (!rendered) {
    const pre = document.createElement("pre");
    pre.textContent = JSON.stringify(value, null, 2);
    details.appendChild(pre);
  }
}
function contentNode(item) {
  if (item.kind === "text") return markdownNode(item.text);
  const details = document.createElement("details");
  details.className = "context-block";
  const summary = document.createElement("summary");
  summary.textContent = item.label || tr("injectedContext");
  const size = document.createElement("span");
  size.className = "context-size";
  size.textContent = byteText(item.bytes || 0);
  summary.appendChild(size);
  details.appendChild(summary);
  details.ontoggle = () => {
    if (!details.open || details.dataset.loaded) return;
    details.dataset.loaded = "1";
    if (typeof item.text === "string") {
      appendContextValue(details, { text: item.text }, item.label);
      return;
    }
    const loading = document.createElement("pre");
    loading.textContent = tr("loading");
    details.appendChild(loading);
    run(async () => {
      const r = await command(
        {
          command: "message_content",
          thread_id: state.current.id,
          message_index: item.message_index,
          content_index: item.content_index,
          content_end: item.content_end ?? null,
        },
        false,
      );
      loading.remove();
      appendContextValue(details, r.content, item.label);
    });
  };
  return details;
}
function toolValueText(value) {
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value, null, 2);
}
function appendToolValue(parent, value) {
  let parsed = value;
  if (typeof parsed === "string") {
    try {
      parsed = JSON.parse(parsed);
    } catch {}
  }
  const values = Array.isArray(parsed)
    ? parsed
    : Array.isArray(parsed?.content)
      ? parsed.content
      : [parsed];
  for (const part of values) {
    let imageUrl = typeof part?.image_url === "string" ? part.image_url : null;
    if (!imageUrl && part?.type === "image" && typeof part.data === "string")
      imageUrl = `data:${part.mimeType || part.mime_type || "image/png"};base64,${part.data}`;
    if (imageUrl && /^(?:data:image\/|https?:\/\/)/i.test(imageUrl)) {
      const img = document.createElement("img");
      img.src = imageUrl;
      img.alt = tr("toolResultImage");
      img.loading = "lazy";
      parent.appendChild(img);
      continue;
    }
    if (typeof part?.audio_url === "string") {
      const audio = document.createElement("audio");
      audio.controls = true;
      audio.src = part.audio_url;
      parent.appendChild(audio);
      continue;
    }
    const raw =
      typeof part?.text === "string"
        ? part.text
        : typeof part?.output_text === "string"
          ? part.output_text
          : null;
    const pre = document.createElement("pre");
    pre.textContent = raw === null ? toolValueText(part) : raw;
    parent.appendChild(pre);
  }
}
function appendPatchDiff(parent, patch) {
  const card = document.createElement("div"),
    head = document.createElement("div"),
    title = document.createElement("span"),
    copy = document.createElement("button"),
    content = document.createElement("pre");
  card.className = "diff-card";
  head.className = "diff-head";
  title.textContent = "Diff";
  copy.className = "diff-copy";
  copy.type = "button";
  copy.textContent = "⧉";
  copy.title = tr("copyPatch");
  copy.setAttribute("aria-label", copy.title);
  copy.onclick = () =>
    run(async () => {
      await navigator.clipboard.writeText(patch);
      notify(tr("diffCopied"));
    });
  head.append(title, copy);
  content.className = "diff-content";
  for (const line of patch.split("\n")) {
    if (["*** Begin Patch", "*** End Patch"].includes(line)) continue;
    const row = document.createElement("span");
    row.className = "diff-line";
    if (/^\*\*\* (?:Add|Update|Delete) File: /.test(line)) row.classList.add("file");
    else if (line.startsWith("@@")) row.classList.add("hunk");
    else if (line.startsWith("+")) row.classList.add("add");
    else if (line.startsWith("-")) row.classList.add("delete");
    row.textContent = line || " ";
    content.appendChild(row);
  }
  card.append(head, content);
  parent.appendChild(card);
}
function toolGroupNode(message, keepRunning = false) {
  const tools = message.tools || [];
  if (!tools.length) return null;
  const group = document.createElement("details");
  group.className = "tool-group";
  const runningTool = tools.findLast((tool) => !toolFinished(tool)),
    latest = runningTool || tools.at(-1),
    summary = document.createElement("summary"),
    icon = document.createElement("span"),
    action = document.createElement("span"),
    preview = document.createElement("span"),
    count = document.createElement("span"),
    running = Boolean(runningTool) || keepRunning;
  summary.className = "tool-group-summary";
  summary.classList.toggle("running", running);
  icon.className = `tool-icon ${toolIconClass(latest.name)}`;
  icon.title = latest.name;
  action.className = "tool-summary-action";
  action.textContent = running ? toolActionText(latest.name, true) : tr("ranTools");
  preview.className = "tool-summary-preview";
  preview.textContent = running
    ? toolSummaryPreview(latest)
    : tr("toolCount", { count: tools.length });
  preview.title = running ? latest.preview || latest.name : preview.textContent;
  count.className = "tool-summary-count";
  const editedFiles = tools.reduce((total, tool) => total + (tool.file_count || 0), 0);
  count.textContent = running
    ? tools.length > 1
      ? `+${tools.length - 1}`
      : ""
    : editedFiles
      ? tr("editedFiles", { count: editedFiles })
      : "";
  summary.append(icon, action, preview, count);
  group.appendChild(summary);
  const list = document.createElement("div");
  list.className = "tool-list";
  for (const tool of tools) {
    const detail = document.createElement("details");
    detail.className = "tool-call";
    const head = document.createElement("summary"),
      icon = document.createElement("span"),
      preview = document.createElement("span"),
      status = document.createElement("span");
    icon.className = `tool-icon ${toolIconClass(tool.name)}`;
    icon.title = tool.name;
    preview.className = "tool-preview";
    preview.appendChild(document.createTextNode(localizedToolPreview(tool)));
    if (tool.additions !== null && tool.additions !== undefined) {
      const add = document.createElement("span"),
        del = document.createElement("span");
      add.className = "tool-add";
      add.textContent = `+${tool.additions}`;
      del.className = "tool-del";
      del.textContent = `−${tool.deletions || 0}`;
      preview.append(add, del);
    }
    status.className = "tool-state";
    status.textContent = tool.has_output ? "✓" : "…";
    head.append(icon, preview, status);
    detail.appendChild(head);
    detail.ontoggle = () => {
      if (!detail.open || detail.dataset.loaded) return;
      detail.dataset.loaded = "1";
      const body = document.createElement("div");
      body.className = "tool-detail";
      body.textContent = tr("loading");
      detail.appendChild(body);
      run(async () => {
        const r = await command(
          {
            command: "tool_content",
            thread_id: state.current.id,
            message_index: message.message_index,
            tool_index: tool.tool_index,
          },
          false,
        );
        body.textContent = "";
        const outputTitle = document.createElement("h5");
        outputTitle.textContent = tr("result");
        if (r.display_input?.operation === "apply_patch" && r.display_input?.patch) {
          appendPatchDiff(body, r.display_input.patch);
          body.appendChild(outputTitle);
        } else {
          const inputTitle = document.createElement("h5");
          inputTitle.textContent = tr("input");
          const input = document.createElement("pre");
          input.textContent = toolValueText(r.display_input);
          body.append(inputTitle, input, outputTitle);
        }
        appendToolValue(body, r.tool.output);
      });
    };
    list.appendChild(detail);
  }
  group.appendChild(list);
  return group;
}
function toolFinished(tool) {
  return tool.has_output || ["completed", "failed", "declined", "cancelled"].includes(tool.status);
}
function toolIconClass(name) {
  if (["exec", "exec_command"].includes(name)) return "exec_command";
  return ["apply_patch", "write_stdin", "web_search"].includes(name) ? name : "other";
}
function toolActionText(name, running) {
  if (running) {
    if (name === "apply_patch") return tr("editing");
    if (name === "write_stdin") return tr("waitingOutput");
    if (name === "web_search") return tr("searching");
    return tr("running");
  }
  if (name === "apply_patch") return tr("edited");
  if (name === "write_stdin") return tr("continued");
  return ["exec", "exec_command"].includes(name) ? tr("ran") : tr("completed");
}
function toolSummaryPreview(tool) {
  if (tool.name === "write_stdin") return "";
  return localizedToolPreview(tool);
}
function localizedToolPreview(tool) {
  const preview = tool.preview || tool.name;
  if (tool.name === "write_stdin") return tr("waitingOutput");
  if (tool.name === "web_search") {
    const match = preview.replace(/^搜索\s+/, "").match(/^(.*) 等 (\d+) 项$/);
    return match
      ? `${match[1]} ${tr("queryCount", { count: match[2] })}`
      : preview.replace(/^搜索\s+/, "");
  }
  if (tool.name === "apply_patch") {
    if (tool.file_count > 1) return tr("filesShort", { count: tool.file_count });
    return preview.replace(/^已(?:编辑|新建|删除|移动)\s+/, "");
  }
  return preview;
}
function messageNode(m, keepToolsRunning = false) {
  const box = document.createElement("article");
  box.className = `message ${m.category || m.role || ""}`;
  const head = document.createElement("div");
  head.className = "message-head";
  const a = document.createElement("span"),
    b = document.createElement("span"),
    date = m.timestamp ? new Date(m.timestamp) : null,
    validDate = date && !Number.isNaN(date.valueOf());
  a.textContent = m.category === "context" ? tr("injectedContext") : m.role || "message";
  b.textContent = validDate
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : m.timestamp || "";
  if (validDate) b.title = date.toLocaleString();
  if (m.role === "assistant") {
    if (b.textContent) head.appendChild(b);
  } else head.append(a, b);
  const body = document.createElement("div");
  body.className = "message-body";
  for (const item of m.content || []) body.appendChild(contentNode(item));
  const tools = toolGroupNode(m, keepToolsRunning);
  if (tools) body.appendChild(tools);
  if (head.childNodes.length) box.appendChild(head);
  box.appendChild(body);
  return box;
}
function pendingNode(entry) {
  const box = document.createElement("article");
  box.className = "outbox-item";
  box.dataset.pendingId = entry.id;
  const body = document.createElement("div");
  body.className = "message-body";
  body.appendChild(markdownNode(entry.text));
  const labels = {
      queueing: tr("queueing"),
      steering: tr("steering"),
      queued: state.usageUnavailable ? tr("queuedRecovering") : tr("queuedWaiting"),
      steered: tr("steeredWaiting"),
      failed: tr("sendFailed"),
    },
    actions = document.createElement("div"),
    status = document.createElement("div"),
    remove = document.createElement("button"),
    busy = ["queueing", "steering"].includes(entry.status);
  actions.className = "outbox-actions";
  status.className = "outbox-status";
  status.textContent = labels[entry.status] || entry.status;
  if (entry.error) status.title = entry.error;
  remove.className = "outbox-delete";
  remove.type = "button";
  remove.textContent = tr("delete");
  remove.disabled = busy;
  remove.title = busy ? tr("deleteBusy") : tr("deleteRestore");
  remove.setAttribute("aria-label", remove.title);
  remove.onclick = () => run(() => deletePending(entry, remove));
  actions.append(status, remove);
  body.appendChild(actions);
  box.appendChild(body);
  return box;
}
function renderPending() {
  const tray = $("outboxTray"),
    entries = state.pending.filter((entry) => entry.thread_id === state.current?.id);
  tray.textContent = "";
  for (const entry of entries) tray.appendChild(pendingNode(entry));
  tray.hidden = !entries.length;
}
async function refreshPending() {
  const r = await command({ command: "pending_messages" }, false);
  state.pending = Array.isArray(r.messages) ? r.messages : [];
  renderPending();
}
async function deletePending(entry, button) {
  if (state.current?.id !== entry.thread_id) throw new Error(tr("pendingWrongSession"));
  button.disabled = true;
  try {
    const r = await command(
      { command: "pending_message_delete", id: entry.id, thread_id: entry.thread_id },
      false,
    );
    if (state.current?.id !== entry.thread_id) return;
    const text = typeof r.text === "string" ? r.text : entry.text;
    $("messageText").value = text;
    saveDraft(entry.thread_id, text, true);
    setSendMode(r.message_action === "queue" ? "send" : "steer", false);
    state.pending = state.pending.filter(
      (item) => item.id !== entry.id || item.thread_id !== entry.thread_id,
    );
    renderPending();
    $("messageText").focus();
    $("messageText").setSelectionRange(text.length, text.length);
    notify(r.queue_deleted ? tr("queueDeletedRestore") : tr("deletedRestore"));
  } finally {
    if (button.isConnected) button.disabled = false;
  }
}
function olderButton() {
  const button = document.createElement("button"),
    label = tr("loadOlder", { count: state.before });
  button.className = "older";
  button.textContent = "↑";
  button.title = label;
  button.setAttribute("aria-label", label);
  button.onclick = () => run(loadOlder);
  return button;
}
async function fetchMessages(before = null, limit = state.pageSize) {
  return command({ command: "messages", thread_id: state.current.id, before, limit }, false);
}
function setSendMode(mode, automatic = false) {
  $("sendMode").value = mode;
  state.modeAutomatic = automatic;
  document.querySelectorAll(".mode-button").forEach((peer) => {
    const active = peer.dataset.mode === mode;
    peer.classList.toggle("active", active);
    peer.setAttribute("aria-pressed", String(active));
  });
}
function renderTransientStatus(active) {
  const root = $("messages"),
    visible = active && state.activityPhase === "compacting",
    existing = root.querySelector(".transient-status");
  if (!visible) {
    existing?.remove();
    return;
  }
  if (existing) return;
  const stickToBottom = root.scrollHeight - root.scrollTop - root.clientHeight < 100,
    status = document.createElement("div");
  status.className = "transient-status";
  status.setAttribute("role", "status");
  status.textContent = tr("compacting");
  root.appendChild(status);
  if (stickToBottom) root.scrollTop = root.scrollHeight;
}
function showActivity() {
  const active = Boolean(state.activeTurnId),
    el = $("runState"),
    label =
      state.activityPhase === "compacting"
        ? tr("sessionCompacting")
        : state.activityPhase === "tool"
          ? tr("toolRunning", { tool: state.activeTool ? ` · ${state.activeTool}` : "" })
          : tr("modelRunning");
  el.classList.toggle("active", active);
  document.querySelector(".composer-shell").classList.toggle("agent-active", active);
  el.textContent = active
    ? state.pendingChanges
      ? tr("hasUpdates", { label })
      : label
    : state.pendingChanges
      ? tr("idleUpdates")
      : tr("idle");
  renderTransientStatus(active);
}
function setUsageUnavailable(error) {
  state.usageUnavailable = true;
  const health = $("usageHealth");
  health.hidden = false;
  health.title = error?.message || error || tr("weeklyUnavailable");
  $("usageState").textContent = "";
  $("composerStatus").hidden = false;
  renderPending();
}
async function refreshComposerStatus() {
  if (!state.current) return;
  const usage = $("usageState"),
    health = $("usageHealth"),
    effort = $("effortState");
  if (!state.directAppServer && state.appServerMode === "desktop_bundled_only") {
    state.usageUnavailable = false;
    state.composerModel = null;
    state.composerEffort = null;
    usage.textContent = "";
    health.hidden = true;
    health.title = "";
    effort.textContent = tr("bundledOnly");
    $("modelPickerBtn").title = tr("bundledOnlyHelp");
    $("modelPickerBtn").disabled = true;
    $("composerStatus").hidden = false;
    renderPending();
    return;
  }
  const threadId = state.current.id;
  let r;
  try {
    r = await command({ command: "composer_status", thread_id: threadId }, false);
  } catch (error) {
    if (state.current?.id === threadId) setUsageUnavailable(error);
    return;
  }
  if (state.current?.id !== threadId) return;
  const weekly = r.weekly_usage;
  state.composerModel = r.model || null;
  state.composerEffort = r.reasoning_effort || null;
  state.usageUnavailable = !weekly;
  usage.textContent = weekly ? tr("weeklyRemaining", { percent: weekly.remaining_percent }) : "";
  usage.title = weekly?.resets_at
    ? tr("resetsAt", {
        time: new Date(weekly.resets_at * 1000).toLocaleString(
          getLanguage() === "zh" ? "zh-CN" : "en",
        ),
      })
    : "";
  health.hidden = Boolean(weekly);
  health.title = r.weekly_usage_error?.message || "";
  effort.textContent = state.composerModel
    ? [state.composerModel, state.composerEffort].filter(Boolean).join(" · ")
    : tr("selectModel");
  $("modelPickerBtn").title = state.composerModel ? effort.textContent : tr("selectModelHelp");
  $("modelPickerBtn").disabled = false;
  $("composerStatus").hidden = !state.current;
  renderPending();
}
function selectedModelOption() {
  return state.modelOptions.find((model) => model.id === $("modelSelect").value);
}
function renderEffortOptions(preferred) {
  const model = selectedModelOption(),
    select = $("effortSelect");
  select.textContent = "";
  for (const effort of model?.efforts || []) {
    const option = document.createElement("option");
    option.value = effort.id;
    option.textContent = effort.id;
    select.appendChild(option);
  }
  const available = (model?.efforts || []).map((effort) => effort.id);
  select.value = available.includes(preferred)
    ? preferred
    : available.includes(model?.default_effort)
      ? model.default_effort
      : available[0] || "";
  renderModelDescription();
}
function renderModelDescription() {
  const model = selectedModelOption(),
    effort = model?.efforts?.find((item) => item.id === $("effortSelect").value),
    parts = [model?.description, effort?.description].filter(Boolean);
  $("modelDescription").textContent = parts.join(" · ");
}
function renderModelOptions() {
  const select = $("modelSelect");
  select.textContent = "";
  for (const model of state.modelOptions) {
    const option = document.createElement("option");
    option.value = model.id;
    option.textContent = model.name;
    select.appendChild(option);
  }
  if (state.modelOptions.some((model) => model.id === state.composerModel))
    select.value = state.composerModel;
  renderEffortOptions(state.composerEffort);
}
function renderWorkspaceDiff(summary) {
  const button = $("workspaceDiff");
  button.classList.toggle("dirty", !summary.clean);
  if (summary.clean) {
    button.textContent = tr("diffClean");
  } else {
    const untracked = summary.untracked_files
      ? tr("untracked", { count: summary.untracked_files })
      : "";
    button.textContent = tr("diffFiles", {
      count: summary.files_changed,
      additions: summary.additions,
      deletions: summary.deletions,
      untracked,
    });
  }
  const baseline = summary.base_branch || summary.base_sha.slice(0, 8);
  button.title = tr("diffBaseline", {
    baseline,
    sha: summary.base_sha.slice(0, 8),
    skipped: summary.untracked_lines_skipped
      ? tr("diffSkipped", { count: summary.untracked_lines_skipped })
      : "",
  });
}
async function refreshWorkspaceDiff(force = false) {
  if (!state.current || state.workspaceDiffPolling) return;
  if (!force && Date.now() - state.lastWorkspaceDiffRefresh < 3000) return;
  const threadId = state.current.id,
    button = $("workspaceDiff");
  state.workspaceDiffPolling = true;
  button.disabled = true;
  try {
    const summary = await command({ command: "workspace_diff", thread_id: threadId }, false);
    if (state.current?.id !== threadId) return;
    state.lastWorkspaceDiffRefresh = Date.now();
    renderWorkspaceDiff(summary);
  } catch (error) {
    if (state.current?.id !== threadId) return;
    button.classList.remove("dirty");
    button.textContent = error.message.startsWith("not_git_repository")
      ? tr("notGit")
      : tr("diffUnavailable");
    button.title = error.message;
  } finally {
    state.workspaceDiffPolling = false;
    button.disabled = false;
  }
}
async function toggleModelPicker() {
  const picker = $("modelPicker");
  if (!picker.hidden) {
    picker.hidden = true;
    return;
  }
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const r = await command({ command: "composer_options" }, false);
  state.modelOptions = Array.isArray(r.models) ? r.models : [];
  if (!state.modelOptions.length) throw new Error(tr("noModels"));
  renderModelOptions();
  picker.hidden = false;
}
async function applyThreadSettings() {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const threadId = state.current.id,
    model = $("modelSelect").value,
    effort = $("effortSelect").value;
  await command({ command: "thread_settings_update", thread_id: threadId, model, effort }, false);
  if (state.current?.id !== threadId) return;
  $("modelPicker").hidden = true;
  await refreshComposerStatus();
  notify(tr("switchedModel", { model, effort }));
}
async function refreshActivity() {
  if (!state.current) return null;
  const threadId = state.current.id,
    r = await command({ command: "thread_activity", thread_id: threadId }, false);
  if (state.current?.id !== threadId) return null;
  const activity = r.activity,
    previous = state.activityFileLen;
  state.activityFileLen = activity.file_len;
  state.activeTurnId = activity.active_turn_id || null;
  state.activityPhase = activity.phase || null;
  state.activeTool = activity.active_tool || null;
  if (!state.activeTurnId && $("sendMode").value === "steer") setSendMode("send", true);
  else if (state.activeTurnId && state.modeAutomatic) setSendMode("steer", true);
  showActivity();
  return { activity, changed: previous !== null && previous !== activity.file_len };
}
async function pollActivity() {
  if (state.polling || !state.current) return;
  state.polling = true;
  try {
    const result = await refreshActivity();
    if (result?.changed) {
      state.pendingChanges = true;
      refreshWorkspaceDiff().catch(() => {});
    }
    if (state.pendingChanges) {
      if (Date.now() - state.lastMessageRefresh < 3500) return;
      const root = $("messages"),
        atBottom = root.scrollHeight - root.scrollTop - root.clientHeight < 100,
        detailsOpen = Boolean(root.querySelector("details[open]"));
      if (atBottom && !detailsOpen) {
        state.pendingChanges = false;
        await openThread(state.current, { quiet: true });
      } else showActivity();
      return;
    }
    await refreshPending();
  } finally {
    state.polling = false;
  }
}
function resetHorizontalPosition() {
  const root = $("messages");
  root.scrollLeft = 0;
  document.documentElement.scrollLeft = 0;
  document.body.scrollLeft = 0;
  window.scrollTo(0, 0);
}
function settleHorizontalPosition() {
  requestAnimationFrame(() => {
    resetHorizontalPosition();
    requestAnimationFrame(resetHorizontalPosition);
  });
}
async function openThread(thread, { quiet = false } = {}) {
  const token = ++state.openToken,
    changedThread = state.current?.id !== thread.id;
  if (!quiet) closePanels();
  if (state.current) saveDraft(state.current.id, $("messageText").value, true);
  state.current = thread;
  $("messageText").value = state.drafts.get(thread.id) || "";
  state.before = null;
  state.hasMore = false;
  if (changedThread) {
    state.activityFileLen = null;
    state.activeTurnId = null;
    state.activityPhase = null;
    state.activeTool = null;
    state.pendingChanges = false;
    state.lastWorkspaceDiffRefresh = 0;
    $("composerStatus").hidden = true;
    $("modelPicker").hidden = true;
    refreshComposerStatus().catch(() => {});
  }
  renderPending();
  if (!quiet) {
    state.userScrolled = false;
    document.activeElement?.blur();
    resetHorizontalPosition();
  }
  renderProjects();
  $("threadTitle").textContent = thread.title || thread.id;
  $("threadMeta").textContent =
    `${thread.cwd} · ${thread.git_branch || tr("noBranch")} · ${thread.id}`;
  const root = $("messages");
  if (!quiet) root.innerHTML = `<div class="empty">${tr("loadingLatest")}</div>`;
  const [r] = await Promise.all([fetchMessages(), refreshActivity()]);
  if (token !== state.openToken) return;
  state.current = { ...thread, ...r.thread };
  state.before = r.page.before;
  state.hasMore = r.page.has_more;
  root.textContent = "";
  if (state.hasMore) root.appendChild(olderButton());
  const activeToolMessage = state.activeTurnId
    ? r.messages.findLast((message) => message.tools?.length)
    : null;
  for (const m of r.messages) root.appendChild(messageNode(m, m === activeToolMessage));
  if (!r.messages.length) root.innerHTML = `<div class="empty">${tr("noMessages")}</div>`;
  root.scrollTop = root.scrollHeight;
  state.pendingChanges = false;
  state.lastMessageRefresh = Date.now();
  await refreshPending();
  await refreshWorkspaceDiff(true);
  if (!quiet) settleHorizontalPosition();
}
async function refreshThread() {
  if (!state.current) return notify(tr("chooseSessionError"), true);
  return openThread(state.current);
}
async function loadOlder() {
  if (!state.current || !state.hasMore || state.loadingHistory) return;
  state.loadingHistory = true;
  state.userScrolled = false;
  const root = $("messages"),
    oldHeight = root.scrollHeight,
    oldTop = root.scrollTop;
  let r;
  try {
    r = await fetchMessages(state.before);
  } finally {
    state.loadingHistory = false;
  }
  state.before = r.page.before;
  state.hasMore = r.page.has_more;
  root.querySelector(".older")?.remove();
  const fragment = document.createDocumentFragment();
  if (state.hasMore) fragment.appendChild(olderButton());
  for (const m of r.messages) fragment.appendChild(messageNode(m));
  root.prepend(fragment);
  root.scrollTop = oldTop + (root.scrollHeight - oldHeight);
}
function targetRequest(name, extra = {}) {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  return { command: name, thread_id: state.current.id, ...extra };
}
async function write(name) {
  const draft = $("messageText").value,
    text = draft.trim();
  if (!text) throw new Error(tr("messageRequired"));
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const shell = document.querySelector(".composer-shell");
  let threadId = null;
  shell.classList.add("submitting");
  try {
    if (name === "steer") {
      const activity = await refreshActivity();
      if (!activity?.activity.active_turn_id) {
        name = "send";
        setSendMode("send", true);
        notify(tr("noActiveTurnQueued"));
      }
    }
    threadId = state.current.id;
    $("messageText").value = "";
    saveDraft(threadId, "", true);
    const request = command(targetRequest(name, { text })).then(
      (value) => ({ value }),
      (error) => ({ error }),
    );
    await new Promise((resolve) => setTimeout(resolve, 40));
    await refreshPending();
    const outcome = await request;
    if (outcome.error) {
      await refreshPending();
      throw outcome.error;
    }
    if (state.current?.id === threadId) await openThread(state.current, { quiet: true });
    else await refreshPending();
    notify(name === "send" ? tr("messageQueued") : tr("guidanceSteered"));
  } catch (error) {
    if (threadId && !state.drafts.has(threadId)) saveDraft(threadId, draft, true);
    if (state.current?.id === threadId && !$("messageText").value)
      $("messageText").value = state.drafts.get(threadId) || "";
    throw error;
  } finally {
    shell.classList.remove("submitting");
  }
}
function approval(name) {
  const id = Number($("approvalId").value);
  if (!Number.isSafeInteger(id) || id < 0) throw new Error(tr("validRequestId"));
  return command({ command: name, id });
}
$("search").oninput = renderProjects;
$("archived").onchange = () => run(loadProjects);
$("reloadBtn").onclick = () => run(loadProjects);
$("statusBtn").onclick = () => run(loadStatus);
$("languageBtn").onclick = () => run(toggleLanguage);
$("refreshBtn").onclick = () => run(refreshThread);
$("createCurrentBtn").onclick = () => run(() => createThread(false));
$("createWorktreeBtn").onclick = () => run(() => createThread(true));
$("createCancelBtn").onclick = closeCreateDialog;
$("createDialog").onclick = (event) => {
  if (event.target === $("createDialog")) closeCreateDialog();
};
$("selectBtn").onclick = () =>
  run(async () => {
    const r = await command(targetRequest("select"));
    notify(tr("selected", { session: r.thread.title || r.thread.id }));
  });
document
  .querySelectorAll(".mode-button")
  .forEach((button) => (button.onclick = () => setSendMode(button.dataset.mode, false)));
$("submitBtn").onclick = () => run(() => write($("sendMode").value));
$("interruptBtn").onclick = () =>
  run(async () => {
    await command(targetRequest("interrupt"));
    notify(tr("turnInterrupted"));
  });
$("mobileInterruptBtn").onclick = () => $("interruptBtn").click();
$("archiveThreadBtn").onclick = () =>
  run(async () => {
    if (!state.current) throw new Error(tr("chooseSessionError"));
    const threadId = state.current.id,
      title = state.current.title || threadId;
    if (!window.confirm(tr("archiveConfirm", { title }))) return;
    await command({ command: "thread_archive", thread_id: threadId }, false);
    state.current = null;
    state.activeTurnId = null;
    state.activeTool = null;
    state.activityPhase = null;
    $("threadTitle").textContent = tr("chooseSession");
    $("threadMeta").textContent = tr("archivedRemoved");
    $("messages").innerHTML = `<div class="empty">${tr("loadingAnother")}</div>`;
    $("messageText").value = "";
    renderPending();
    closePanels();
    await loadProjects();
    notify(tr("sessionArchived"));
  });
$("usageHealth").onclick = () => notify($("usageHealth").title || tr("weeklyUnavailable"), true);
$("usageHealth").onkeydown = (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    $("usageHealth").click();
  }
};
$("workspaceDiff").onclick = () => refreshWorkspaceDiff(true);
$("themeSelect").onchange = (event) => applyTheme(event.target.value);
$("modelPickerBtn").onclick = () => run(toggleModelPicker);
$("modelPickerClose").onclick = () => ($("modelPicker").hidden = true);
$("modelSelect").onchange = () => renderEffortOptions(state.composerEffort);
$("effortSelect").onchange = renderModelDescription;
$("modelApply").onclick = () => run(applyThreadSettings);
$("messageText").oninput = () => saveDraft(state.current?.id, $("messageText").value);
$("messageText").onkeydown = (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") $("submitBtn").click();
};
$("currentBtn").onclick = () =>
  run(async () => {
    const r = await command({ command: "current" });
    await openThread(r.thread);
  });
$("tailBtn").onclick = () => run(() => command({ command: "tail" }));
$("pendingBtn").onclick = () => run(() => command({ command: "pending" }));
$("applyLastBtn").onclick = () => {
  state.pageSize = Number($("lastCount").value);
  run(refreshThread);
};
$("scrollBtn").onclick = () =>
  run(() => {
    const mode = $("scrollMode").value,
      v = $("scrollValue").value,
      req = { command: "scroll", direction: null, pixels: null, target: null, message_id: null };
    req[mode] = mode === "pixels" ? Number(v) : v;
    return command(req);
  });
$("approveBtn").onclick = () => run(() => approval("approve"));
$("declineBtn").onclick = () => run(() => approval("decline"));
$("execBtn").onclick = () =>
  run(async () => {
    let argv;
    try {
      argv = JSON.parse($("argv").value);
    } catch {
      throw new Error(tr("argvValidJson"));
    }
    if (!Array.isArray(argv) || !argv.every((x) => typeof x === "string"))
      throw new Error(tr("argvStringArray"));
    const timeout = $("timeout").value;
    return command(
      targetRequest("host_exec", { argv, timeout_seconds: timeout ? Number(timeout) : null }),
    );
  });
$("rpcBtn").onclick = () =>
  run(() => {
    const method = $("rpcMethod").value.trim();
    if (!method) throw new Error(tr("methodRequired"));
    let params;
    try {
      params = JSON.parse($("rpcParams").value);
    } catch {
      throw new Error(tr("paramsValidJson"));
    }
    return command({ command: "app_server_rpc", method, params });
  });
$("rawBtn").onclick = () =>
  run(() => {
    let req;
    try {
      req = JSON.parse($("rawRequest").value);
    } catch {
      throw new Error(tr("requestValidJson"));
    }
    return command(req);
  });
function syncScrim() {
  $("scrim").classList.toggle(
    "show",
    $("tools").classList.contains("open") || $("sidebar").classList.contains("open"),
  );
}
function closePanels() {
  $("tools").classList.remove("open");
  $("sidebar").classList.remove("open");
  syncScrim();
}
function bindSwipe(element, direction, onSwipe, { ignoreInteractive = false } = {}) {
  let gesture = null;
  element.addEventListener(
    "touchstart",
    (event) => {
      if (!matchMedia("(max-width:700px)").matches || event.touches.length !== 1) return;
      if (
        !ignoreInteractive &&
        event.target.closest(
          '.composer,button,input,textarea,select,a,pre,[contenteditable="true"]',
        )
      )
        return;
      const touch = event.touches[0];
      gesture = { x: touch.clientX, y: touch.clientY, horizontal: false };
    },
    { passive: true },
  );
  element.addEventListener(
    "touchmove",
    (event) => {
      if (!gesture || event.touches.length !== 1) return;
      const touch = event.touches[0],
        dx = touch.clientX - gesture.x,
        dy = touch.clientY - gesture.y;
      if (!gesture.horizontal && Math.abs(dx) > 12 && Math.abs(dx) > Math.abs(dy) * 1.35)
        gesture.horizontal = true;
      if (gesture.horizontal) event.preventDefault();
    },
    { passive: false },
  );
  element.addEventListener(
    "touchend",
    (event) => {
      if (!gesture) return;
      const touch = event.changedTouches[0],
        dx = touch.clientX - gesture.x,
        dy = touch.clientY - gesture.y;
      if (
        gesture.horizontal &&
        Math.abs(dx) > 64 &&
        Math.abs(dx) > Math.abs(dy) * 1.35 &&
        Math.sign(dx) === direction
      )
        onSwipe();
      gesture = null;
    },
    { passive: true },
  );
  element.addEventListener(
    "touchcancel",
    () => {
      gesture = null;
    },
    { passive: true },
  );
}
bindSwipe(document.querySelector("main"), 1, () => {
  if (!$("tools").classList.contains("open")) {
    $("sidebar").classList.add("open");
    syncScrim();
  }
});
bindSwipe(
  $("sidebar"),
  -1,
  () => {
    $("sidebar").classList.remove("open");
    syncScrim();
  },
  { ignoreInteractive: true },
);
window.addEventListener("pagehide", persistDrafts);
document.addEventListener("click", (event) => {
  if (
    !$("modelPicker").hidden &&
    !$("modelPicker").contains(event.target) &&
    !$("modelPickerBtn").contains(event.target)
  )
    $("modelPicker").hidden = true;
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !$("createDialog").hidden) closeCreateDialog();
});
$("messages").addEventListener("touchmove", () => (state.userScrolled = true), { passive: true });
$("messages").addEventListener("wheel", () => (state.userScrolled = true), { passive: true });
$("messages").onscroll = () => {
  if (state.userScrolled && $("messages").scrollTop < 8 && !state.loadingHistory && state.hasMore)
    run(loadOlder);
};
document.querySelectorAll(".tool-toggle").forEach(
  (b) =>
    (b.onclick = () => {
      $("tools").classList.toggle("open");
      syncScrim();
    }),
);
document.querySelectorAll(".nav-toggle").forEach(
  (b) =>
    (b.onclick = () => {
      $("sidebar").classList.toggle("open");
      syncScrim();
    }),
);
$("scrim").onclick = closePanels;
applyLanguage(localStorage.getItem(LANGUAGE_STORAGE_KEY) || "en", false);
applyTheme(localStorage.getItem(THEME_STORAGE_KEY) || "dark");
run(async () => {
  await loadStatus();
  await loadProjects();
});
setInterval(() => pollActivity().catch(() => {}), 1500);
setInterval(() => refreshComposerStatus().catch(() => {}), 60000);
