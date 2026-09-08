import "./styles.css";
import {
  $,
  authenticate,
  command,
  createFileDownloadTicket,
  demoMode,
  notify,
  run,
  subscribeEvents,
  timeText,
} from "./api.js";
import { applyLanguage, getLanguage, LANGUAGE_STORAGE_KEY, t as tr } from "./i18n.js";
import { markdownNode } from "./markdown.js";
import { shouldOfferStop } from "./composer-state.js";
import {
  rememberSessionId,
  sessionHash,
  sessionIdFromHash,
  storedSessionId,
} from "./session-route.js";
import {
  DRAFT_STORAGE_KEY,
  applyTheme,
  persistDrafts,
  saveDraft,
  state,
  storedTheme,
  watchSystemTheme,
} from "./state.js";
document.documentElement.toggleAttribute("data-demo", demoMode);

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
  const base = r.demo
    ? tr("demoReady", { protocol: r.protocol_version })
    : tr("bridgeReady", {
        protocol: r.protocol_version,
        backend: appServer,
      });
  const schemaVersion = backend.schema_version,
    runtimeVersion = backend.runtime_version,
    runtimeBase = runtimeVersion?.replace(/-(?:bundled|standalone)$/, ""),
    mismatch = Boolean(schemaVersion && runtimeBase && schemaVersion !== runtimeBase);
  $("bridgeState").textContent = [
    base,
    schemaVersion ? `schema ${schemaVersion}` : "",
    runtimeVersion ? `app-server ${runtimeVersion}` : "",
  ]
    .filter(Boolean)
    .join(" · ");
  $("bridgeState").classList.toggle("version-mismatch", mismatch);
  $("bridgeState").title = mismatch
    ? `Schema ${schemaVersion} does not match app-server ${runtimeVersion}`
    : backend.runtime_user_agent || "";
}
function setThreadHeaderExpanded(expanded) {
  if (!matchMedia("(max-width:800px)").matches) expanded = false;
  const header = document.querySelector(".thread-head"),
    title = $("threadTitle");
  header.classList.toggle("expanded", expanded);
  title.setAttribute("aria-expanded", String(expanded));
  title.title = tr(expanded ? "collapseHeader" : "expandHeader");
}
async function toggleLanguage() {
  applyLanguage(getLanguage() === "en" ? "zh" : "en");
  syncComposerPlaceholder();
  setSendMode($("sendMode").value, state.modeAutomatic);
  renderProjects();
  renderPending();
  renderComposerAttachments();
  showActivity();
  await loadStatus();
  if (state.current) await openThread(state.current, { quiet: true });
}
async function loadProjects() {
  const [r] = await Promise.all([
    command({ command: "projects", include_archived: $("archived").checked }, false),
    loadPins(),
  ]);
  state.projects = r.projects || [];
  state.projectThreads.clear();
  state.expanded.clear();
  renderProjects();
  if (!state.current) {
    const hashedId = sessionIdFromHash(window.location.hash),
      requestedId = hashedId || storedSessionId(window.localStorage);
    if (requestedId) {
      try {
        await openSessionById(requestedId, {
          fromHash: Boolean(hashedId),
          replaceHash: !hashedId,
        });
        return;
      } catch {
        state.current = null;
      }
    }
    if (state.projects.length) {
      const project = state.projects[0];
      state.expanded.add(project.path);
      const threads = await loadProjectThreads(project);
      if (threads.length) await openThread(threads[0], { replaceHash: true });
    }
  }
}
async function loadPins() {
  if (!state.directAppServer) {
    state.pinAvailable = false;
    state.pinnedIds.clear();
    state.pinnedThreads = [];
    return;
  }
  try {
    const result = await command({ command: "thread_pins" }, false),
      ids = Array.isArray(result.thread_ids) ? result.thread_ids : [];
    state.pinAvailable = Boolean(result.available);
    state.pinnedIds = new Set(ids);
    const returned = new Map(
      (Array.isArray(result.threads) ? result.threads : []).map((thread) => [thread.id, thread]),
    );
    for (const data of state.projectThreads.values()) {
      for (const thread of data.threads) {
        if (!returned.has(thread.id)) returned.set(thread.id, thread);
      }
    }
    state.pinnedThreads = ids
      .map((id) => returned.get(id))
      .filter(Boolean)
      .map((thread) => ({ ...thread, pinned: true }));
    const ranks = new Map(ids.map((id, rank) => [id, rank]));
    for (const data of state.projectThreads.values()) {
      for (const thread of data.threads) thread.pinned = state.pinnedIds.has(thread.id);
      data.threads.sort(
        (left, right) =>
          (ranks.get(left.id) ?? Number.MAX_SAFE_INTEGER) -
          (ranks.get(right.id) ?? Number.MAX_SAFE_INTEGER),
      );
    }
  } catch {
    state.pinAvailable = false;
    state.pinnedIds.clear();
    state.pinnedThreads = [];
  }
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
async function toggleThreadPin(thread) {
  if (!state.pinAvailable || state.pinBusy.has(thread.id)) return;
  const pinned = !thread.pinned,
    project = pinnedProject(thread);
  state.pinBusy.add(thread.id);
  renderProjects();
  try {
    await command({ command: "thread_pin", thread_id: thread.id, pinned }, false);
    if (pinned) state.pinnedIds.add(thread.id);
    else state.pinnedIds.delete(thread.id);
    await loadPins();
    if (!pinned && project && state.projectThreads.has(project.path)) {
      await loadProjectThreads(project);
    }
    notify(tr(pinned ? "sessionPinned" : "sessionUnpinned"));
  } finally {
    state.pinBusy.delete(thread.id);
    renderProjects();
  }
}
function threadMatches(thread, query) {
  return `${thread.title || ""} ${thread.id} ${thread.cwd || ""} ${thread.git_branch || ""}`
    .toLowerCase()
    .includes(query);
}
function threadRow(thread) {
  const row = document.createElement("div");
  row.className = `thread-row${state.pinAvailable && !thread.archived ? "" : " no-pin"}`;
  const button = document.createElement("button");
  button.className = "thread" + (state.current?.id === thread.id ? " active" : "");
  button.innerHTML = '<div class="thread-name"></div><div class="thread-meta"></div>';
  const name = document.createElement("span");
  name.textContent = thread.title || thread.id;
  button.children[0].appendChild(name);
  if (state.updatingThreads.has(thread.id)) {
    const live = document.createElement("span");
    live.className = "thread-live";
    live.setAttribute("aria-label", tr("modelRunning"));
    live.title = tr("modelRunning");
    button.children[0].appendChild(live);
  }
  button.children[1].textContent = `${thread.git_branch || tr("noBranch")} · ${timeText(thread.updated_at_ms, getLanguage() === "zh" ? "zh-CN" : "en")}${thread.archived ? ` · ${tr("archived")}` : ""}`;
  button.onclick = () => openThread(thread);
  row.appendChild(button);
  if (state.pinAvailable && !thread.archived) {
    const pin = document.createElement("button"),
      label = tr(thread.pinned ? "unpinSession" : "pinSession");
    pin.className = `thread-pin${thread.pinned ? " pinned" : ""}`;
    pin.type = "button";
    pin.innerHTML =
      '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M9 3h6l-1 6 3 3v2H7v-2l3-3-1-6Zm3 11v7" /></svg>';
    pin.title = label;
    pin.setAttribute("aria-label", label);
    pin.setAttribute("aria-pressed", String(Boolean(thread.pinned)));
    pin.disabled = state.pinBusy.has(thread.id);
    pin.onclick = () => run(() => toggleThreadPin(thread));
    row.appendChild(pin);
  }
  return row;
}
function pinnedProject(thread) {
  const cwd = String(thread.cwd || "").replace(/\/+$/, "");
  return state.projects
    .filter((project) => {
      const path = String(project.path).replace(/\/+$/, "");
      return cwd === path || cwd.startsWith(`${path}/`);
    })
    .sort((left, right) => String(right.path).length - String(left.path).length)[0];
}
function updateSessionHash(threadId, replace = false) {
  const hash = sessionHash(threadId);
  if (window.location.hash === hash) return;
  window.history[replace ? "replaceState" : "pushState"](null, "", hash);
}
async function revealSessionProject(thread) {
  if (state.pinnedIds.has(thread.id)) return renderProjects();
  const project = pinnedProject(thread);
  if (!project) return renderProjects();
  state.expanded.add(project.path);
  if (!state.projectThreads.has(project.path)) await loadProjectThreads(project);
  else renderProjects();
}
async function openSessionById(threadId, { fromHash = false, replaceHash = false } = {}) {
  const previous = state.current;
  const known = state.pinnedThreads.find((thread) => thread.id === threadId) ||
    [...state.projectThreads.values()]
      .flatMap((entry) => entry.threads)
      .find((thread) => thread.id === threadId) || {
      id: threadId,
      title: threadId,
      cwd: "",
      git_branch: null,
    };
  try {
    await openThread(known, { writeHash: !fromHash, replaceHash });
    await revealSessionProject(state.current);
  } catch (error) {
    state.current = previous;
    renderProjects();
    if (previous) await openThread(previous, { quiet: true, writeHash: false });
    throw error;
  }
}
function renderProjects() {
  const q = $("search").value.trim().toLowerCase(),
    root = $("projects");
  root.textContent = "";
  const pinned = state.pinnedThreads.filter((thread) => threadMatches(thread, q));
  const projects = state.projects.filter(
    (p) =>
      `${p.name} ${p.path}`.toLowerCase().includes(q) ||
      state.projectThreads
        .get(p.path)
        ?.threads.some((t) => !state.pinnedIds.has(t.id) && threadMatches(t, q)),
  );
  if (!projects.length && !pinned.length) {
    root.innerHTML = `<div class="empty" style="padding:12px">${tr("noMatchingProjects")}</div>`;
    return;
  }
  if (pinned.length) {
    const section = document.createElement("section"),
      heading = document.createElement("div"),
      list = document.createElement("div");
    section.className = "pinned-section";
    heading.className = "pinned-heading";
    heading.innerHTML = '<span class="pinned-title"></span><span class="pinned-count"></span>';
    heading.children[0].textContent = tr("pinnedSessions");
    heading.children[1].textContent = `${pinned.length}`;
    list.className = "pinned-threads";
    for (const thread of pinned) list.appendChild(threadRow(thread));
    section.append(heading, list);
    root.appendChild(section);
  }
  for (const p of projects) {
    const wrap = document.createElement("section");
    wrap.className = "project";
    const head = document.createElement("div");
    head.className = "project-head";
    const button = document.createElement("button");
    button.className = "project-button";
    button.innerHTML =
      '<span class="chevron"></span><span class="project-name"></span><span class="project-count"></span>';
    button.children[0].textContent = state.expanded.has(p.path) ? "▾" : "▸";
    button.children[1].textContent = p.name;
    const pinnedCount = state.pinnedThreads.filter(
      (thread) => pinnedProject(thread)?.path === p.path,
    ).length;
    button.children[2].textContent = `${Math.max(0, p.thread_count - pinnedCount)}`;
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
        if (!state.pinnedIds.has(t.id)) list.appendChild(threadRow(t));
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
const MAX_COMPOSER_ATTACHMENTS = 6;
const MAX_ATTACHMENT_DATA_URL_BYTES = 6 * 1024 * 1024;
let voiceRecorder = null;
let voiceStream = null;
let voiceChunks = [];
let voiceLimitTimer = null;
function blobDataUrl(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error || new Error(tr("imageReadFailed")));
    reader.readAsDataURL(blob);
  });
}
async function imageAttachment(file) {
  if (file.type === "image/gif") {
    const url = await blobDataUrl(file);
    if (url.length > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("imageTooLarge"));
    return { type: "image", url, name: file.name };
  }
  let bitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error(tr("imageReadFailed"));
  }
  const scale = Math.min(1, 2048 / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  const context = canvas.getContext("2d");
  context.fillStyle = "#fff";
  context.fillRect(0, 0, canvas.width, canvas.height);
  context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close?.();
  const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/webp", 0.88));
  if (!blob) throw new Error(tr("imageReadFailed"));
  const url = await blobDataUrl(blob);
  if (url.length > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("imageTooLarge"));
  return { type: "image", url, name: file.name };
}
function renderComposerAttachments() {
  const tray = $("composerAttachments");
  tray.replaceChildren();
  state.composerAttachments.forEach((attachment, index) => {
    const item = document.createElement("div");
    item.className = `composer-attachment ${attachment.type}`;
    if (attachment.type === "image") {
      const image = document.createElement("img");
      image.src = attachment.url;
      image.alt = attachment.name || tr("attachment");
      item.appendChild(image);
    } else {
      item.innerHTML =
        '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M5 9v6M9 6v12M13 4v16M17 7v10M21 10v4"/></svg>';
      item.title = attachment.name || tr("recordVoice");
    }
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "attachment-remove";
    remove.textContent = "×";
    remove.title = tr("removeAttachment");
    remove.setAttribute("aria-label", remove.title);
    remove.onclick = () => {
      state.composerAttachments.splice(index, 1);
      renderComposerAttachments();
    };
    item.appendChild(remove);
    tray.appendChild(item);
  });
  tray.hidden = !state.composerAttachments.length;
  resizeComposerTextarea();
}
async function addImages(files) {
  const available = MAX_COMPOSER_ATTACHMENTS - state.composerAttachments.length;
  if (files.length > available) throw new Error(tr("attachmentLimit"));
  for (const file of files) state.composerAttachments.push(await imageAttachment(file));
  renderComposerAttachments();
}
async function addAudioFile(file) {
  if (state.composerAttachments.length >= MAX_COMPOSER_ATTACHMENTS)
    throw new Error(tr("attachmentLimit"));
  const fallbackType = /\.m4a$/i.test(file.name) ? "audio/mp4" : "audio/webm";
  const blob = file.type ? file : file.slice(0, file.size, fallbackType);
  const url = await blobDataUrl(blob);
  if (url.length > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("voiceTooLarge"));
  state.composerAttachments.push({ type: "audio", url, name: file.name || "voice" });
  renderComposerAttachments();
}
function resetVoiceRecorder() {
  clearTimeout(voiceLimitTimer);
  voiceLimitTimer = null;
  voiceStream?.getTracks().forEach((track) => track.stop());
  voiceStream = null;
  voiceRecorder = null;
  voiceChunks = [];
  $("voiceBtn").classList.remove("recording");
  $("voiceBtn").title = tr("recordVoice");
  $("voiceBtn").setAttribute("aria-label", $("voiceBtn").title);
}
async function toggleVoiceRecording() {
  if (voiceRecorder?.state === "recording") {
    voiceRecorder.stop();
    return;
  }
  if (state.composerAttachments.length >= MAX_COMPOSER_ATTACHMENTS)
    throw new Error(tr("attachmentLimit"));
  if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) {
    $("audioInput").click();
    return;
  }
  try {
    voiceStream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const preferred = ["audio/mp4", "audio/webm;codecs=opus", "audio/webm"].find((type) =>
      MediaRecorder.isTypeSupported(type),
    );
    voiceRecorder = preferred
      ? new MediaRecorder(voiceStream, { mimeType: preferred })
      : new MediaRecorder(voiceStream);
    voiceChunks = [];
    voiceRecorder.ondataavailable = (event) => event.data.size && voiceChunks.push(event.data);
    voiceRecorder.onerror = () => {
      voiceRecorder.onstop = null;
      resetVoiceRecorder();
      notify(tr("microphoneUnavailable"), true);
    };
    voiceRecorder.onstop = async () => {
      const mime = voiceRecorder?.mimeType || voiceChunks[0]?.type || "audio/webm";
      const blob = new Blob(voiceChunks, { type: mime });
      try {
        const url = await blobDataUrl(blob);
        if (url.length > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("voiceTooLarge"));
        state.composerAttachments.push({
          type: "audio",
          url,
          name: `voice-${new Date().toISOString()}`,
        });
        resetVoiceRecorder();
        renderComposerAttachments();
      } catch (error) {
        resetVoiceRecorder();
        notify(error.message, true);
      }
    };
    voiceRecorder.start(250);
    $("voiceBtn").classList.add("recording");
    $("voiceBtn").title = tr("stopRecording");
    $("voiceBtn").setAttribute("aria-label", $("voiceBtn").title);
    voiceLimitTimer = setTimeout(() => {
      if (voiceRecorder?.state === "recording") voiceRecorder.stop();
    }, 120000);
    notify(tr("voiceRecording"));
  } catch (error) {
    resetVoiceRecorder();
    throw new Error(error?.message || tr("microphoneUnavailable"));
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
  if (item.kind === "text") return markdownNode(item.text, markdownOptions(state.current?.id));
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
function markdownOptions(threadId) {
  return {
    requestLocalFileDownload: async (path) => {
      if (!threadId) throw new Error(tr("chooseSessionError"));
      const ticket = await createFileDownloadTicket(threadId, path);
      if (!ticket?.url) throw new Error("download ticket response is invalid");
      return ticket.url;
    },
    onError: (error) => notify(error.message, true),
  };
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
function appendPatchDiff(parent, patch, label = "Diff") {
  const card = document.createElement("div"),
    head = document.createElement("div"),
    title = document.createElement("span"),
    copy = document.createElement("button"),
    content = document.createElement("pre");
  card.className = "diff-card";
  head.className = "diff-head";
  title.textContent = label;
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
    if (/^(?:\*\*\* (?:Add|Update|Delete) File: |--- |\+\+\+ )/.test(line))
      row.classList.add("file");
    else if (line.startsWith("@@")) row.classList.add("hunk");
    else if (line.startsWith("+")) row.classList.add("add");
    else if (line.startsWith("-")) row.classList.add("delete");
    row.textContent = line || " ";
    content.appendChild(row);
  }
  card.append(head, content);
  parent.appendChild(card);
}

function appendCommandActions(parent, input) {
  const actions = Array.isArray(input?.commandActions) ? input.commandActions : [];
  if (!actions.length) return false;
  const title = document.createElement("h5");
  const parallel =
    input.parallel === true ||
    ["parallel", "concurrent"].includes(input.executionMode || input.execution_mode);
  title.textContent = parallel
    ? tr("parallelCommands", { count: actions.length })
    : tr("commandActions", { count: actions.length });
  const list = document.createElement("div");
  list.className = `command-actions${parallel ? " parallel" : ""}`;
  for (const [index, action] of actions.entries()) {
    const item = document.createElement("section"),
      head = document.createElement("div"),
      number = document.createElement("span"),
      kind = document.createElement("span"),
      target = document.createElement("span"),
      command = document.createElement("pre");
    item.className = "command-action";
    head.className = "command-action-head";
    number.className = "command-action-number";
    number.textContent = String(index + 1);
    kind.className = "command-action-kind";
    kind.textContent = action?.type || tr("command");
    target.className = "command-action-target";
    target.textContent = action?.name || action?.path || action?.query || "";
    command.textContent = action?.command || toolValueText(action);
    head.append(number, kind);
    if (target.textContent) head.appendChild(target);
    item.append(head, command);
    list.appendChild(item);
  }
  parent.append(title, list);
  return true;
}
const renderedMessageState = new WeakMap();

function toolGroupNode(message, keepRunning = false) {
  const tools = message.tools || [];
  if (!tools.length) return null;
  const group = document.createElement("details");
  group.className = "tool-group";
  const runningTool = tools.findLast((tool) => !toolFinished(tool)),
    latest = runningTool || tools.at(-1),
    summary = document.createElement("summary"),
    icon = document.createElement("span"),
    label = document.createElement("span"),
    running = Boolean(runningTool) || keepRunning;
  summary.className = "tool-group-summary";
  summary.classList.toggle("running", running);
  icon.className = `tool-icon ${toolIconClass(latest.name)}`;
  icon.title = latest.name;
  const editedFiles = tools.reduce((total, tool) => total + (tool.file_count || 0), 0);
  label.className = "tool-summary-label";
  label.textContent = running
    ? [
        toolActionText(latest.name, true),
        toolSummaryPreview(latest),
        tools.length > 1 ? `+${tools.length - 1}` : "",
      ]
        .filter(Boolean)
        .join(" ")
    : [
        tr("ranTools"),
        tr("toolCount", { count: tools.length }),
        editedFiles ? tr("editedFiles", { count: editedFiles }) : "",
      ]
        .filter(Boolean)
        .join(" ");
  label.title = running ? latest.preview || latest.name : label.textContent;
  summary.append(icon, label);
  group.appendChild(summary);
  const list = document.createElement("div");
  list.className = "tool-list";
  for (const [toolPosition, tool] of tools.entries()) {
    const detail = document.createElement("details");
    detail.className = "tool-call";
    detail.dataset.toolIndex = String(tool.tool_index ?? toolPosition);
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
    status.textContent = toolFinished(tool) ? "✓" : "…";
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
        if (r.display_input?.type === "fileChange" && Array.isArray(r.display_input?.changes)) {
          for (const change of r.display_input.changes) {
            if (typeof change?.diff === "string")
              appendPatchDiff(body, change.diff, change.path || "Diff");
          }
          body.appendChild(outputTitle);
        } else if (r.display_input?.operation === "apply_patch" && r.display_input?.patch) {
          appendPatchDiff(body, r.display_input.patch);
          body.appendChild(outputTitle);
        } else {
          const inputTitle = document.createElement("h5");
          inputTitle.textContent = tr("input");
          const input = document.createElement("pre");
          if (appendCommandActions(body, r.display_input)) {
            body.appendChild(outputTitle);
          } else {
            input.textContent =
              typeof r.display_input?.command === "string"
                ? r.display_input.command
                : toolValueText(r.display_input);
            body.append(inputTitle, input, outputTitle);
          }
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
  if (tool.name === "exec_command" && tool.command_action_count > 1) {
    const mode = tool.command_actions_parallel ? "parallelCommandsShort" : "commandActionsShort";
    return `${preview} · ${tr(mode, { count: tool.command_action_count })}`;
  }
  return preview;
}
function messageNode(m, keepToolsRunning = false) {
  const box = document.createElement("article");
  box.className = `message ${m.category || m.role || ""}`;
  box.dataset.messageIndex = String(m.message_index);
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
  const copyText = (m.content || [])
    .filter((item) => item.kind === "text" && typeof item.text === "string")
    .map((item) => item.text)
    .join("\n\n");
  if (copyText) {
    const copy = document.createElement("button");
    copy.className = "message-copy";
    copy.type = "button";
    copy.innerHTML =
      '<svg aria-hidden="true" viewBox="0 0 24 24"><rect x="8" y="8" width="11" height="11" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg>';
    copy.title = tr("copyMessage");
    copy.setAttribute("aria-label", copy.title);
    copy.onclick = () =>
      run(async () => {
        await navigator.clipboard.writeText(copyText);
        notify(tr("messageCopied"));
      });
    box.appendChild(copy);
  }
  renderedMessageState.set(box, {
    signature: JSON.stringify(m),
    keepToolsRunning,
  });
  return box;
}

function preserveLoadedToolDetails(previous, next) {
  const previousGroup = previous.querySelector(".tool-group"),
    nextGroup = next.querySelector(".tool-group");
  if (!previousGroup || !nextGroup) return;
  nextGroup.open = previousGroup.open;
  const previousTools = new Map(
    [...previousGroup.querySelectorAll(".tool-call")].map((detail) => [
      detail.dataset.toolIndex,
      detail,
    ]),
  );
  for (const detail of nextGroup.querySelectorAll(".tool-call")) {
    const previousDetail = previousTools.get(detail.dataset.toolIndex);
    if (!previousDetail) continue;
    detail.open = previousDetail.open;
    const loadedBody = previousDetail.querySelector(":scope > .tool-detail");
    if (!loadedBody) continue;
    detail.dataset.loaded = "1";
    detail.appendChild(loadedBody);
  }
}

function reconcileMessageNodes(root, response, activeToolMessage) {
  const existing = new Map(
      [...root.querySelectorAll(":scope > .message[data-message-index]")].map((message) => [
        message.dataset.messageIndex,
        message,
      ]),
    ),
    fragment = document.createDocumentFragment();
  if (response.page.has_more) fragment.appendChild(olderButton());
  for (const message of response.messages) {
    const key = String(message.message_index),
      previous = existing.get(key),
      keepToolsRunning = message === activeToolMessage,
      previousState = previous ? renderedMessageState.get(previous) : null,
      signature = JSON.stringify(message);
    if (
      previous &&
      previousState?.signature === signature &&
      previousState.keepToolsRunning === keepToolsRunning
    ) {
      fragment.appendChild(previous);
      continue;
    }
    const next = messageNode(message, keepToolsRunning);
    if (previous) preserveLoadedToolDetails(previous, next);
    fragment.appendChild(next);
  }
  if (!response.messages.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = tr("noMessages");
    fragment.appendChild(empty);
  }
  root.replaceChildren(fragment);
}
function pendingNode(entry) {
  const box = document.createElement("article");
  const submitting = ["queueing", "steering"].includes(entry.status);
  box.className = `outbox-item${entry.handoff ? " handoff" : ""}${submitting ? " submitting" : ""}`;
  box.dataset.pendingId = entry.id;
  const body = document.createElement("div");
  body.className = "message-body";
  body.appendChild(markdownNode(entry.text, markdownOptions(entry.thread_id || state.current?.id)));
  if (!entry.handoff && ["queue", "steer"].includes(entry.action)) {
    const mode = document.createElement("div");
    mode.className = "outbox-mode";
    mode.textContent = tr(entry.action === "steer" ? "followUp" : "queue");
    body.appendChild(mode);
  }
  const actions = document.createElement("div"),
    busy = ["queueing", "steering"].includes(entry.status);
  actions.className = "outbox-actions";
  if (entry.handoff) {
    const status = document.createElement("div");
    status.className = "outbox-status";
    status.textContent = tr(entry.status === "processing" ? "processingShort" : "acceptedShort");
    status.title = tr(entry.status === "processing" ? "handoffProcessing" : "serverAccepted");
    actions.appendChild(status);
  } else {
    const remove = document.createElement("button");
    remove.className = "outbox-delete";
    remove.type = "button";
    remove.textContent = tr("withdraw");
    remove.disabled = busy;
    remove.title = busy ? tr("deleteBusy") : tr("deleteRestore");
    remove.setAttribute("aria-label", remove.title);
    remove.onclick = () => run(() => deletePending(entry, remove));
    actions.appendChild(remove);
  }
  box.append(actions, body);
  return box;
}
function renderPending() {
  const tray = $("outboxTray"),
    entries = state.pending.filter((entry) => entry.thread_id === state.current?.id);
  tray.textContent = "";
  for (const entry of entries) tray.appendChild(pendingNode(entry));
  const delivery = state.delivery?.threadId === state.current?.id ? state.delivery : null,
    pendingStillVisible = delivery?.pendingId
      ? entries.some((entry) => entry.id === delivery.pendingId)
      : false;
  if (
    delivery?.text &&
    !delivery.onScreen &&
    !pendingStillVisible &&
    ["accepted", "processing"].includes(delivery.phase)
  ) {
    tray.appendChild(
      pendingNode({
        id: `handoff-${delivery.threadId}`,
        text: delivery.text,
        status: delivery.phase,
        handoff: true,
      }),
    );
  }
  tray.hidden = !tray.childElementCount;
  syncOutboxCompactLabel();
}
function syncOutboxCompactLabel() {
  const tray = $("outboxTray"),
    label = tr(tray.classList.contains("compact") ? "expandPending" : "collapsePending");
  tray.title = label;
  tray.setAttribute("aria-label", label);
}
async function refreshPending() {
  const r = await command(
    { command: "pending_messages", thread_id: state.current?.id || null },
    false,
  );
  state.pending = Array.isArray(r.messages) ? r.messages : [];
  if (
    state.delivery?.pendingId &&
    state.delivery.phase === "queued" &&
    !state.pending.some((entry) => entry.id === state.delivery.pendingId)
  ) {
    setDeliveryState("accepted");
  }
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
    resizeComposerTextarea();
    saveDraft(entry.thread_id, text, true);
    setSendMode(r.message_action === "queue" ? "send" : "steer", false);
    state.pending = state.pending.filter(
      (item) => item.id !== entry.id || item.thread_id !== entry.thread_id,
    );
    if (state.delivery?.pendingId === entry.id) setDeliveryState(null);
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
// Message indices come from the current rollout parse, not immutable event IDs. Continue a
// page only while its boundary is exact; otherwise rebuild the visible region atomically.
function isValidMessagePage(result, { latest = false, expectedEnd = null } = {}) {
  const page = result?.page,
    messages = result?.messages;
  if (!page || !Array.isArray(messages)) return false;
  const bounds = [page.start, page.end, page.total];
  if (
    !bounds.every(Number.isSafeInteger) ||
    page.start < 0 ||
    page.start > page.end ||
    page.end > page.total ||
    page.end - page.start !== messages.length ||
    page.before !== page.start ||
    (latest && page.end !== page.total) ||
    (expectedEnd !== null && page.end !== expectedEnd)
  )
    return false;
  return messages.every((message, offset) => message.message_index === page.start + offset);
}
function requireMessagePage(result, options) {
  if (!isValidMessagePage(result, options)) throw new Error(tr("historyOutOfSync"));
  return result;
}
function applyMessagePageState(result) {
  state.lastMessageIndex = result.messages.length ? result.page.end - 1 : null;
  state.before = result.page.before;
  state.hasMore = result.page.has_more;
  state.historyStart = result.page.start;
  state.historyEnd = result.page.end;
  state.historyTotal = result.page.total;
}
function setSendMode(mode, automatic = false) {
  $("sendMode").value = mode;
  state.modeAutomatic = automatic;
  const button = $("sendModeToggle"),
    isSteer = mode === "steer";
  button.dataset.mode = mode;
  button.textContent = tr(isSteer ? "followUp" : "queue");
  button.title = tr(isSteer ? "switchToQueue" : "switchToSteer");
  button.setAttribute("aria-label", button.title);
}
let deliveryClearTimer = null;
function setDeliveryState(phase, details = {}) {
  clearTimeout(deliveryClearTimer);
  const previousPhase = state.delivery?.phase;
  state.delivery = phase
    ? {
        ...(state.delivery || {}),
        ...details,
        phase,
        announcedPhase: phase === previousPhase ? state.delivery?.announcedPhase : null,
      }
    : null;
  showActivity();
  if (phase === "completed" || phase === "failed") {
    deliveryClearTimer = setTimeout(() => {
      state.delivery = null;
      showActivity();
    }, 1800);
  }
}
function transientStatus(active) {
  const delivery =
    state.delivery?.threadId === state.current?.id && !state.delivery.dismissed
      ? state.delivery
      : null;
  if (!delivery) return null;
  if (delivery.phase === "submitting") return ["submitting", tr("submittingToServer")];
  if (delivery.phase === "queued") return ["queued", tr("serverQueued")];
  if (delivery.phase === "accepted" && !delivery.started) return ["accepted", tr("serverAccepted")];
  if (delivery.phase === "processing" && active) return ["processing", tr("handoffProcessing")];
  if (delivery.phase === "failed") return ["failed", tr("requestFailed")];
  return null;
}
function usesDocumentMessageScroll() {
  return matchMedia("(max-width:800px)").matches;
}
function messageScrollMetrics() {
  const root = $("messages");
  if (usesDocumentMessageScroll()) {
    return {
      top: window.scrollY,
      height: document.documentElement.scrollHeight,
      client: window.innerHeight,
    };
  }
  return { top: root.scrollTop, height: root.scrollHeight, client: root.clientHeight };
}
function scrollMessagesToBottom() {
  if (usesDocumentMessageScroll()) window.scrollTo(0, document.documentElement.scrollHeight);
  else $("messages").scrollTop = $("messages").scrollHeight;
}
function renderTransientStatus(active) {
  const root = $("messages"),
    statusState = transientStatus(active),
    existing = root.querySelector(".transient-status");
  if (!statusState) {
    existing?.remove();
    return;
  }
  if (!existing && state.delivery?.announcedPhase === statusState[0]) return;
  const metrics = messageScrollMetrics(),
    stickToBottom = metrics.height - metrics.top - metrics.client < 100,
    status = existing || document.createElement("div");
  status.className = "transient-status";
  status.setAttribute("role", "button");
  status.setAttribute("tabindex", "0");
  status.setAttribute("aria-live", "polite");
  status.dataset.phase = statusState[0];
  status.textContent = statusState[1];
  status.title = tr("dismissStatus");
  status.setAttribute("aria-label", `${statusState[1]}. ${tr("dismissStatus")}`);
  status.onclick = () => {
    if (state.delivery) state.delivery.dismissed = true;
    status.remove();
  };
  status.onkeydown = (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      status.click();
    }
  };
  if (!existing) root.appendChild(status);
  if (state.delivery) state.delivery.announcedPhase = statusState[0];
  if (stickToBottom) scrollMessagesToBottom();
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
  syncSubmitAction();
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
  button.hidden = Boolean(summary.clean);
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
  button.title = tr("diffWorkingTree", {
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
    button.hidden = false;
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
  const delivery = state.delivery?.threadId === threadId ? state.delivery : null,
    pendingDelivery = delivery?.pendingId
      ? state.pending.some((entry) => entry.id === delivery.pendingId)
      : false;
  if (
    state.activeTurnId &&
    delivery &&
    (delivery.mode === "steer" ||
      (state.activeTurnId !== delivery.initialTurnId && !pendingDelivery)) &&
    ["accepted", "queued"].includes(delivery.phase)
  ) {
    state.delivery = { ...delivery, phase: "processing", started: true };
  } else if (!state.activeTurnId && delivery?.started && delivery.phase === "processing") {
    setDeliveryState("completed");
  }
  renderPending();
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
      if (state.delivery?.phase === "processing") state.delivery.dismissed = true;
      showActivity();
      refreshWorkspaceDiff().catch(() => {});
    }
    if (state.pendingChanges) {
      if (Date.now() - state.lastMessageRefresh < 3500) return;
      await openThread(state.current, { quiet: true, preserveView: true });
      return;
    }
    await refreshPending();
  } finally {
    state.polling = false;
  }
}
let eventRefreshTimer = null;
function scheduleEventRefresh(threadId, immediate = false) {
  if (threadId !== state.current?.id) return;
  state.pendingChanges = true;
  clearTimeout(eventRefreshTimer);
  eventRefreshTimer = setTimeout(
    () => {
      eventRefreshTimer = null;
      pollActivity().catch(() => {});
    },
    immediate ? 0 : 280,
  );
}
function handleBridgeEvent(event) {
  if (event?.type === "bridge_event_gap") {
    state.lastMessageRefresh = 0;
    scheduleEventRefresh(state.current?.id, true);
    loadProjects().catch(() => {});
    return;
  }
  if (event?.type === "bridge_app_server_connection") {
    loadStatus().catch(() => {});
    return;
  }
  if (event?.type === "bridge_thread_activity_snapshot") {
    if (!Array.isArray(event.active_thread_ids)) return;
    state.updatingThreads.clear();
    for (const threadId of event.active_thread_ids) {
      if (typeof threadId === "string" && threadId) state.updatingThreads.add(threadId);
    }
    renderProjects();
    return;
  }
  if (event?.type !== "app_server") return;
  const message = event.message || {},
    method = message.method,
    params = message.params || {},
    threadId = params.threadId || null;
  if (!method) return;
  if (method === "thread/status/changed" && threadId) {
    updateThreadLiveFromStatus(threadId, params.status);
  } else if (method === "turn/started" && threadId) {
    state.updatingThreads.add(threadId);
    renderProjects();
    if (threadId === state.current?.id) {
      state.activeTurnId = params.turn?.id || state.activeTurnId;
      const delivery = state.delivery?.threadId === threadId ? state.delivery : null;
      if (delivery && ["queued", "accepted"].includes(delivery.phase)) {
        state.delivery = { ...delivery, phase: "processing", started: true };
      }
      showActivity();
    }
  } else if (method === "turn/completed" && threadId) {
    state.updatingThreads.delete(threadId);
    renderProjects();
    if (threadId === state.current?.id) {
      state.activeTurnId = null;
      if (state.delivery?.started) setDeliveryState("completed");
      showActivity();
    }
  }
  if (method === "thread/queue/changed" && threadId === state.current?.id) {
    refreshPending().catch(() => {});
  }
  if (
    threadId === state.current?.id &&
    (method.startsWith("turn/") ||
      method.startsWith("item/") ||
      method === "thread/status/changed" ||
      method === "thread/reverted")
  ) {
    scheduleEventRefresh(threadId, method === "turn/completed");
  }
}
function updateThreadLiveFromStatus(threadId, status) {
  if (status?.type === "active") state.updatingThreads.add(threadId);
  else if (status) state.updatingThreads.delete(threadId);
  renderProjects();
}
function resetHorizontalPosition() {
  const root = $("messages");
  root.scrollLeft = 0;
  document.documentElement.scrollLeft = 0;
  document.body.scrollLeft = 0;
  window.scrollTo(0, window.scrollY);
}
function resizeComposerTextarea() {
  const textarea = $("messageText"),
    composer = document.querySelector(".composer"),
    shell = document.querySelector(".composer-shell"),
    threadHead = document.querySelector(".thread-head");
  if (!textarea || !composer || !shell || !threadHead) return;
  shell.classList.toggle("has-text", Boolean(textarea.value || state.composerAttachments.length));
  syncComposerPlaceholder();
  textarea.style.height = "44px";
  const viewportHeight = window.visualViewport?.height || window.innerHeight,
    composerChrome = Math.max(0, composer.getBoundingClientRect().height - 44),
    available = Math.max(
      44,
      viewportHeight - threadHead.getBoundingClientRect().height - composerChrome - 16,
    ),
    desktopLimit = Math.min(320, viewportHeight * 0.34),
    limit = usesDocumentMessageScroll() ? available : desktopLimit,
    height = Math.min(Math.max(44, textarea.scrollHeight), limit);
  textarea.style.height = `${Math.ceil(height)}px`;
  textarea.style.overflowY = textarea.scrollHeight > height + 1 ? "auto" : "hidden";
}
function syncComposerPlaceholder() {
  const textarea = $("messageText"),
    shell = document.querySelector(".composer-shell"),
    compact =
      usesDocumentMessageScroll() &&
      !textarea.value &&
      document.activeElement !== textarea &&
      !shell.classList.contains("input-focused");
  textarea.placeholder = tr(compact ? "messagePlaceholderCompact" : "messagePlaceholder");
}
function settleHorizontalPosition() {
  requestAnimationFrame(() => {
    resetHorizontalPosition();
    requestAnimationFrame(resetHorizontalPosition);
  });
}
function captureMessageView() {
  const root = $("messages"),
    metrics = messageScrollMetrics();
  return {
    atBottom: metrics.height - metrics.top - metrics.client < 100,
    top: metrics.top,
    openDetails: [...root.querySelectorAll(".message details[open]")].map((detail) => {
      const message = detail.closest(".message");
      return `${message?.dataset.messageIndex || ""}:${[
        ...message.querySelectorAll("details"),
      ].indexOf(detail)}`;
    }),
  };
}
function restoreMessageView(view) {
  if (!view) return scrollMessagesToBottom();
  const openDetails = new Set(view.openDetails);
  for (const message of $("messages").querySelectorAll(".message")) {
    [...message.querySelectorAll("details")].forEach((detail, index) => {
      detail.open = openDetails.has(`${message.dataset.messageIndex || ""}:${index}`);
    });
  }
  if (view.atBottom) scrollMessagesToBottom();
  else if (usesDocumentMessageScroll()) window.scrollTo(0, view.top);
  else $("messages").scrollTop = view.top;
}
async function openThread(
  thread,
  { quiet = false, preserveView = false, writeHash = true, replaceHash = false } = {},
) {
  const token = ++state.openToken,
    changedThread = state.current?.id !== thread.id,
    messageView = quiet && preserveView && !changedThread ? captureMessageView() : null;
  if (!quiet) {
    closePanels();
    setThreadHeaderExpanded(false);
  }
  if (state.current) {
    saveDraft(state.current.id, $("messageText").value, true);
    state.attachmentDrafts.set(state.current.id, state.composerAttachments);
  }
  state.current = thread;
  $("messageText").value = state.drafts.get(thread.id) || "";
  state.composerAttachments = state.attachmentDrafts.get(thread.id) || [];
  renderComposerAttachments();
  if (changedThread) {
    state.before = null;
    state.hasMore = false;
    state.activityFileLen = null;
    state.activeTurnId = null;
    state.activityPhase = null;
    state.activeTool = null;
    state.pendingChanges = false;
    state.lastMessageIndex = null;
    state.historyStart = null;
    state.historyEnd = null;
    state.historyTotal = null;
    state.lastWorkspaceDiffRefresh = 0;
    $("composerStatus").hidden = true;
    $("modelPicker").hidden = true;
  }
  refreshComposerStatus().catch(() => {});
  renderPending();
  if (!quiet) {
    state.userScrolled = false;
    document.activeElement?.blur();
    resetHorizontalPosition();
  }
  renderProjects();
  $("threadTitle").textContent = thread.title || thread.id;
  $("threadCompactMeta").textContent = thread.git_branch || tr("noBranch");
  $("threadMeta").textContent =
    `${thread.cwd} · ${thread.git_branch || tr("noBranch")} · ${thread.id}`;
  const root = $("messages");
  const cached = changedThread ? state.messageCache.get(thread.id) : null;
  if (!quiet) {
    if (cached && isValidMessagePage(cached, { latest: true })) {
      root.replaceChildren();
      reconcileMessageNodes(root, cached, null);
      applyMessagePageState(cached);
      requestAnimationFrame(scrollMessagesToBottom);
    } else {
      root.innerHTML = `<div class="empty">${tr("loadingLatest")}</div>`;
    }
  }
  const [r] = await Promise.all([
    fetchMessages(),
    refreshActivity(),
    demoMode
      ? Promise.resolve()
      : command({ command: "thread_watch", thread_id: thread.id }, false)
          .then((watch) => updateThreadLiveFromStatus(thread.id, watch.thread?.status))
          .catch(() => {}),
  ]);
  if (token !== state.openToken) return;
  requireMessagePage(r, { latest: true });
  state.messageCache.set(thread.id, r);
  state.current = { ...thread, ...r.thread };
  rememberSessionId(window.localStorage, state.current.id);
  if (writeHash) updateSessionHash(state.current.id, replaceHash);
  const delivery = state.delivery?.threadId === thread.id ? state.delivery : null;
  if (
    delivery?.text &&
    r.messages.some(
      (message) =>
        message.role === "user" &&
        message.message_index > (delivery.initialMessageIndex ?? -1) &&
        message.content?.some(
          (item) => item.kind === "text" && item.text?.trim() === delivery.text.trim(),
        ),
    )
  ) {
    delivery.onScreen = true;
  }
  const activeToolMessage = state.activeTurnId
    ? r.messages.findLast((message) => message.tools?.length)
    : null;
  reconcileMessageNodes(root, r, activeToolMessage);
  applyMessagePageState(r);
  renderPending();
  restoreMessageView(messageView);
  state.pendingChanges = false;
  state.lastMessageRefresh = Date.now();
  await refreshPending();
  showActivity();
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
  const threadId = state.current.id,
    openToken = state.openToken,
    expectedEnd = state.historyStart ?? state.before,
    root = $("messages"),
    metrics = messageScrollMetrics(),
    oldHeight = metrics.height,
    oldTop = metrics.top;
  let r;
  try {
    r = await fetchMessages(state.before);
  } finally {
    state.loadingHistory = false;
  }
  if (state.current?.id !== threadId || state.openToken !== openToken) return;
  if (
    !isValidMessagePage(r, { expectedEnd }) ||
    (state.historyTotal !== null && r.page.total < state.historyTotal)
  ) {
    await openThread(state.current, { quiet: true });
    notify(tr("historyResynced"));
    return;
  }
  state.before = r.page.before;
  state.hasMore = r.page.has_more;
  state.historyStart = r.page.start;
  state.historyTotal = Math.max(state.historyTotal ?? 0, r.page.total);
  root.querySelector(".older")?.remove();
  const fragment = document.createDocumentFragment();
  if (state.hasMore) fragment.appendChild(olderButton());
  for (const m of r.messages) fragment.appendChild(messageNode(m));
  root.prepend(fragment);
  const newHeight = messageScrollMetrics().height;
  if (usesDocumentMessageScroll()) window.scrollTo(0, oldTop + (newHeight - oldHeight));
  else root.scrollTop = oldTop + (newHeight - oldHeight);
}
function targetRequest(name, extra = {}) {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  return { command: name, thread_id: state.current.id, ...extra };
}
function setComposerSubmitting(active) {
  const shell = document.querySelector(".composer-shell");
  shell.classList.toggle("submitting", active);
  $("messageText").disabled = active;
  $("sendModeToggle").disabled = active;
  $("attachBtn").disabled = active;
  $("voiceBtn").disabled = active;
  $("imageInput").disabled = active;
  $("audioInput").disabled = active;
  $("submitBtn").disabled = active;
  syncSubmitAction();
}
function syncSubmitAction() {
  const shell = document.querySelector(".composer-shell"),
    stopReady = shouldOfferStop({
      activeTurnId: state.activeTurnId,
      inputFocused: shell.classList.contains("input-focused"),
      submitting: shell.classList.contains("submitting"),
      interrupting: state.interrupting,
    }),
    button = $("submitBtn");
  shell.classList.toggle("stop-ready", stopReady);
  shell.classList.toggle("interrupting", state.interrupting);
  button.dataset.action = stopReady ? "interrupt" : "submit";
  button.title = tr(stopReady ? "stopRunAria" : "submitAria");
  button.setAttribute("aria-label", button.title);
  syncComposerPlaceholder();
}
async function interruptCurrentRun({ confirm = true, requireActive = false } = {}) {
  if ((requireActive && !state.activeTurnId) || state.interrupting) return;
  if (confirm && !window.confirm(tr("stopRunConfirm"))) return;
  state.interrupting = true;
  $("submitBtn").disabled = true;
  syncSubmitAction();
  try {
    await command(targetRequest("interrupt"));
    notify(tr("turnInterrupted"));
    await refreshActivity();
  } finally {
    state.interrupting = false;
    $("submitBtn").disabled = false;
    syncSubmitAction();
  }
}
async function write(name) {
  const draft = $("messageText").value,
    text = draft.trim(),
    attachments = state.composerAttachments.map((attachment) => ({ ...attachment }));
  if (!text && !attachments.length) throw new Error(tr("messageRequired"));
  if (voiceRecorder?.state === "recording") throw new Error(tr("stopRecording"));
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const threadId = state.current.id;
  let acknowledged = false;
  setDeliveryState("submitting", {
    threadId,
    mode: name,
    pendingId: null,
    initialTurnId: state.activeTurnId,
    initialMessageIndex: state.lastMessageIndex,
    started: false,
    dismissed: false,
    onScreen: false,
    text: text || `${attachments.length} ${tr("attachment")}`,
  });
  setComposerSubmitting(true);
  try {
    if (name === "steer") {
      const activity = await refreshActivity();
      if (!activity?.activity.active_turn_id) {
        name = "send";
        setSendMode("send", true);
        notify(tr("noActiveTurnQueued"));
      }
    }
    $("messageText").value = "";
    resizeComposerTextarea();
    saveDraft(threadId, "", true);
    const request = command({ command: name, thread_id: threadId, text, attachments }).then(
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
    setComposerSubmitting(false);
    acknowledged = true;
    state.composerAttachments = [];
    state.attachmentDrafts.delete(threadId);
    renderComposerAttachments();
    setDeliveryState(outcome.value.status === "queued" ? "queued" : "accepted", {
      mode: name,
      pendingId: outcome.value.pending_id || null,
    });
    try {
      if (state.current?.id === threadId) await openThread(state.current, { quiet: true });
      else await refreshPending();
    } catch {
      notify(tr("acceptedRefreshFailed"), true);
      return;
    }
    notify(name === "send" ? tr("messageQueued") : tr("guidanceSteered"));
  } catch (error) {
    if (!acknowledged) {
      setDeliveryState("failed", { threadId });
      if (!state.drafts.has(threadId)) saveDraft(threadId, draft, true);
      if (state.current?.id === threadId && !$("messageText").value)
        $("messageText").value = state.drafts.get(threadId) || "";
      resizeComposerTextarea();
    }
    throw error;
  } finally {
    setComposerSubmitting(false);
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
$("attachBtn").onclick = () => $("imageInput").click();
$("imageInput").onchange = (event) => {
  const files = [...event.target.files];
  event.target.value = "";
  run(() => addImages(files));
};
$("audioInput").onchange = (event) => {
  const [file] = event.target.files;
  event.target.value = "";
  if (file) run(() => addAudioFile(file));
};
$("voiceBtn").onclick = () => run(toggleVoiceRecording);
$("createCancelBtn").onclick = closeCreateDialog;
$("createDialog").onclick = (event) => {
  if (event.target === $("createDialog")) closeCreateDialog();
};
$("selectBtn").onclick = () =>
  run(async () => {
    const r = await command(targetRequest("select"));
    notify(tr("selected", { session: r.thread.title || r.thread.id }));
  });
function toggleSendModeAndKeepFocus() {
  setSendMode($("sendMode").value === "steer" ? "send" : "steer", false);
  $("messageText").focus({ preventScroll: true });
}
$("sendModeToggle").onpointerdown = (event) => {
  if (!event.isPrimary || event.button !== 0) return;
  event.preventDefault();
  toggleSendModeAndKeepFocus();
};
$("sendModeToggle").onclick = (event) => {
  if (event.detail !== 0) return;
  toggleSendModeAndKeepFocus();
};
$("submitBtn").onpointerdown = (event) => {
  if (!event.isPrimary || event.button !== 0) return;
  event.preventDefault();
  $("submitBtn").dataset.pointerAction ||= $("submitBtn").dataset.action;
};
$("submitBtn").onpointercancel = () => delete $("submitBtn").dataset.pointerAction;
$("submitBtn").onclick = () => {
  const action = $("submitBtn").dataset.pointerAction || $("submitBtn").dataset.action;
  delete $("submitBtn").dataset.pointerAction;
  run(() =>
    action === "interrupt"
      ? interruptCurrentRun({ requireActive: true })
      : write($("sendMode").value),
  );
};
$("interruptBtn").onclick = () => run(() => interruptCurrentRun());
$("mobileInterruptBtn").onclick = () => $("interruptBtn").click();
$("renameThreadBtn").onclick = () =>
  run(async () => {
    if (!state.current) throw new Error(tr("chooseSessionError"));
    const currentName = state.current.title || "";
    const entered = window.prompt(tr("renamePrompt"), currentName);
    if (entered === null) return;
    const name = entered.trim();
    if (!name || [...name].length > 200) throw new Error(tr("renameInvalid"));
    const threadId = state.current.id;
    await command({ command: "thread_rename", thread_id: threadId, name }, false);
    if (state.current?.id === threadId) {
      state.current.title = name;
      $("threadTitle").textContent = name;
    }
    await loadProjects();
    notify(tr("sessionRenamed"));
  });
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
    $("threadCompactMeta").textContent = "";
    $("threadMeta").textContent = tr("archivedRemoved");
    $("messages").innerHTML = `<div class="empty">${tr("loadingAnother")}</div>`;
    $("messageText").value = "";
    state.composerAttachments = [];
    state.attachmentDrafts.delete(threadId);
    renderComposerAttachments();
    resizeComposerTextarea();
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
$("messageText").oninput = () => {
  saveDraft(state.current?.id, $("messageText").value);
  resizeComposerTextarea();
};
$("messageText").onpointerdown = () => (
  document.querySelector(".composer-shell").classList.add("focused", "input-focused"),
  syncSubmitAction()
);
$("messageText").onfocus = () => {
  document.querySelector(".composer-shell").classList.add("focused", "input-focused");
  syncSubmitAction();
  requestAnimationFrame(resizeComposerTextarea);
};
$("messageText").onblur = () =>
  requestAnimationFrame(() => {
    const shell = document.querySelector(".composer-shell");
    if (document.activeElement !== $("messageText")) shell.classList.remove("input-focused");
    syncSubmitAction();
  });
document.querySelector(".composer-shell").addEventListener("focusout", () =>
  requestAnimationFrame(() => {
    const shell = document.querySelector(".composer-shell");
    if (!shell.contains(document.activeElement)) shell.classList.remove("focused");
    syncSubmitAction();
  }),
);
$("messageText").onkeydown = (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
    e.preventDefault();
    $("submitBtn").click();
  }
};
$("threadTitle").onclick = () =>
  setThreadHeaderExpanded(!document.querySelector(".thread-head").classList.contains("expanded"));
$("threadTitle").onkeydown = (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    $("threadTitle").click();
  }
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
      if (!matchMedia("(max-width:800px)").matches || event.touches.length !== 1) return;
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
window.addEventListener("pagehide", () => {
  persistDrafts();
  voiceStream?.getTracks().forEach((track) => track.stop());
});
const refreshAfterResume = () => {
  if (!state.current || document.visibilityState === "hidden") return;
  state.pendingChanges = true;
  state.lastMessageRefresh = 0;
  pollActivity().catch(() => {});
};
document.addEventListener("visibilitychange", refreshAfterResume);
window.addEventListener("focus", refreshAfterResume);
window.addEventListener("online", refreshAfterResume);
window.addEventListener("hashchange", () => {
  const hashedId = sessionIdFromHash(window.location.hash),
    threadId = hashedId || storedSessionId(window.localStorage);
  if (!threadId) return;
  if (threadId === state.current?.id) {
    if (!hashedId) updateSessionHash(threadId, true);
    return;
  }
  run(() => openSessionById(threadId, { fromHash: Boolean(hashedId), replaceHash: !hashedId }));
});
const composer = document.querySelector(".composer"),
  composerShell = document.querySelector(".composer-shell"),
  mainPanel = document.querySelector("main"),
  threadHead = document.querySelector(".thread-head"),
  syncFrameInsets = () => {
    mainPanel.style.setProperty(
      "--composer-height",
      `${Math.ceil(composer.getBoundingClientRect().height)}px`,
    );
    mainPanel.style.setProperty(
      "--thread-head-height",
      `${Math.ceil(threadHead.getBoundingClientRect().height)}px`,
    );
  };
const frameResizeObserver = new ResizeObserver(syncFrameInsets);
frameResizeObserver.observe(composer);
frameResizeObserver.observe(threadHead);
syncFrameInsets();
resizeComposerTextarea();
window.addEventListener("resize", resizeComposerTextarea, { passive: true });
window.visualViewport?.addEventListener("resize", resizeComposerTextarea, { passive: true });
composerShell.addEventListener(
  "pointerdown",
  (event) => {
    if (
      !usesDocumentMessageScroll() ||
      !event.isPrimary ||
      event.button !== 0 ||
      event.target === $("messageText") ||
      event.target.closest(".composer-actions")
    )
      return;
    if ($("submitBtn").contains(event.target)) {
      $("submitBtn").dataset.pointerAction ||= $("submitBtn").dataset.action;
    }
    event.preventDefault();
    $("messageText").focus({ preventScroll: true });
  },
  true,
);
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
$("outboxTray").addEventListener("click", (event) => {
  if (event.target !== event.currentTarget) return;
  $("outboxTray").classList.toggle("compact");
  syncOutboxCompactLabel();
});
syncOutboxCompactLabel();
$("messages").addEventListener("touchmove", () => (state.userScrolled = true), { passive: true });
$("messages").addEventListener("wheel", () => (state.userScrolled = true), { passive: true });
const handleMessageScroll = () => {
  if (
    state.userScrolled &&
    messageScrollMetrics().top < 8 &&
    !state.loadingHistory &&
    state.hasMore
  )
    run(loadOlder);
};
$("messages").onscroll = handleMessageScroll;
window.addEventListener("scroll", handleMessageScroll, { passive: true });
$("messages").addEventListener("pointerdown", () => setThreadHeaderExpanded(false), {
  passive: true,
});
document
  .querySelector(".composer")
  .addEventListener("pointerdown", () => setThreadHeaderExpanded(false));
document
  .querySelector(".composer")
  .addEventListener("focusin", () => setThreadHeaderExpanded(false));
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
setSendMode("steer", false);
applyTheme(storedTheme(), false);
watchSystemTheme();
run(async () => {
  await authenticate();
  subscribeEvents(handleBridgeEvent);
  await loadStatus();
  await loadProjects();
});
setInterval(() => pollActivity().catch(() => {}), 1500);
setInterval(() => refreshComposerStatus().catch(() => {}), 60000);
