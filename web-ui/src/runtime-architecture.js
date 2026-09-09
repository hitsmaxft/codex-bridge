function servicePhase(service, externallyAvailable = false) {
  const status = service?.status || {};
  if (!service?.enabled) {
    return {
      phase: externallyAvailable ? "running" : "external",
      labelKey: "componentExternal",
    };
  }
  if (service.fallback && !service.needed) {
    return { phase: "standby", labelKey: "componentStandby" };
  }
  if (status.running) return { phase: "running", labelKey: "componentRunning" };
  if (status.last_error) return { phase: "failed", labelKey: "componentStopped" };
  return { phase: "stopped", labelKey: "componentStopped" };
}

export function runtimeArchitectureModel({ managedServices, directAppServer, audioTranscription }) {
  const services = managedServices || {},
    appServer = servicePhase(services.app_server, directAppServer),
    wsBridge = servicePhase(services.desktop_interposition),
    whisper = servicePhase(services.whisper),
    voiceBackend = audioTranscription?.backend,
    voiceEnabled = Boolean(audioTranscription?.enabled);
  let voice = {
    phase: "stopped",
    labelKey: "voiceBackendUnavailable",
    nameKey: "voiceBackend",
  };
  if (voiceBackend === "app_server_realtime") {
    voice = {
      phase: voiceEnabled ? "running" : "stopped",
      labelKey: voiceEnabled ? "componentRunning" : "componentStopped",
      nameKey: "appServerRealtime",
    };
  } else if (voiceBackend === "whisper_cpp") {
    voice = {
      phase: voiceEnabled ? "running" : whisper.phase,
      labelKey: voiceEnabled ? "componentRunning" : whisper.labelKey,
      nameKey: "whisperBackend",
    };
  } else if (voiceBackend === "demo_wasm") {
    voice = {
      phase: voiceEnabled ? "running" : "stopped",
      labelKey: voiceEnabled ? "componentRunning" : "componentStopped",
      nameKey: "demoVoiceBackend",
    };
  }
  return { appServer, wsBridge, whisper, voice };
}

function architectureNode(name, status, translate, fixedPhase = null, fixedLabelKey = null) {
  const node = document.createElement("div"),
    label = document.createElement("span"),
    meta = document.createElement("span");
  node.className = `architecture-node ${fixedPhase || status.phase}`;
  label.className = "architecture-node-name";
  label.textContent = name;
  meta.className = "architecture-node-state";
  meta.textContent = fixedPhase
    ? translate(fixedLabelKey || "componentRunning")
    : translate(status.labelKey);
  node.append(label, meta);
  return node;
}

function architectureArrow(label) {
  const arrow = document.createElement("span");
  arrow.className = "architecture-arrow";
  arrow.textContent = "→";
  arrow.setAttribute("aria-label", label);
  arrow.title = label;
  return arrow;
}

function architectureLane(title, nodes, translate) {
  const lane = document.createElement("section"),
    heading = document.createElement("div"),
    flow = document.createElement("div");
  lane.className = "architecture-lane";
  heading.className = "architecture-lane-title";
  heading.textContent = translate(title);
  flow.className = "architecture-lane-flow";
  nodes.forEach((entry, index) => {
    if (index) flow.appendChild(architectureArrow(translate(entry.routeKey)));
    flow.appendChild(
      architectureNode(
        translate(entry.nameKey),
        entry.status,
        translate,
        entry.fixed,
        entry.fixedLabelKey,
      ),
    );
  });
  lane.append(heading, flow);
  return lane;
}

export function renderRuntimeArchitecture(root, snapshot, translate) {
  if (!root) return;
  root.textContent = "";
  if (!snapshot.managedServices) return;
  const model = runtimeArchitectureModel({
      managedServices: snapshot.managedServices,
      directAppServer: snapshot.directAppServer,
      audioTranscription: snapshot.serverCapabilities?.audio_transcription,
    }),
    figure = document.createElement("figure"),
    caption = document.createElement("figcaption"),
    title = document.createElement("strong"),
    hint = document.createElement("span");
  figure.className = "runtime-architecture-figure";
  title.textContent = translate("runtimeArchitecture");
  hint.textContent = translate("runtimeArchitectureHint");
  caption.append(title, hint);
  figure.append(
    caption,
    architectureLane(
      "controlPath",
      [
        {
          nameKey: "webClients",
          status: {},
          fixed: "running",
          fixedLabelKey: "connectedClient",
        },
        { nameKey: "bridgeDaemon", status: {}, fixed: "running", routeKey: "typedRequestRoute" },
        { nameKey: "appServerComponent", status: model.appServer, routeKey: "appServerRpcRoute" },
      ],
      translate,
    ),
    architectureLane(
      "desktopPath",
      [
        {
          nameKey: "codexDesktop",
          status: {},
          fixed: "external",
          fixedLabelKey: "externalClient",
        },
        { nameKey: "wsBridgeShort", status: model.wsBridge, routeKey: "tcpWebSocketRoute" },
        { nameKey: "appServerComponent", status: model.appServer, routeKey: "unixWebSocketRoute" },
      ],
      translate,
    ),
    architectureLane(
      "voicePath",
      [
        {
          nameKey: "microphone",
          status: {},
          fixed: "external",
          fixedLabelKey: "browserInput",
        },
        { nameKey: model.voice.nameKey, status: model.voice, routeKey: "pcmAudioRoute" },
        {
          nameKey: "composer",
          status: {},
          fixed: "running",
          fixedLabelKey: "editableText",
          routeKey: "transcriptRoute",
        },
      ],
      translate,
    ),
  );
  root.appendChild(figure);
}
