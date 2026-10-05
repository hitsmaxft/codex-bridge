const MAX_PARSED_BYTES = 256 * 1024;
const MAX_LIVE_CHARS = 200_000;

export function isCommandTool(name) {
  return name === "exec_command" || name === "exec" || name === "write_stdin";
}

function unwrapText(value) {
  if (typeof value === "string") return value;
  if (Array.isArray(value)) {
    const parts = value.map(unwrapText);
    return parts.every((part) => part !== null) ? parts.join("\n") : null;
  }
  if (Array.isArray(value?.content)) return unwrapText(value.content);
  if (value?.type === "text" && typeof value.text === "string") return value.text;
  return null;
}

function parsedJson(text) {
  if (!text || text.length > MAX_PARSED_BYTES) return null;
  const trimmed = text.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return null;
  try {
    return JSON.parse(trimmed);
  } catch {
    return null;
  }
}

export function commandOutputModel(value, status = "completed") {
  let output = value;
  const wrappedText = unwrapText(output);
  if (typeof wrappedText === "string") {
    const decoded = parsedJson(wrappedText);
    if (decoded !== null) output = decoded;
  }
  const native =
    output &&
    !Array.isArray(output) &&
    typeof output === "object" &&
    ("aggregatedOutput" in output || "exitCode" in output || "durationMs" in output);
  const wrapped =
    !native &&
    output &&
    !Array.isArray(output) &&
    typeof output === "object" &&
    typeof output.output === "string" &&
    ("exit_code" in output || "wall_time_seconds" in output || "session_id" in output);
  let text = native ? output.aggregatedOutput || "" : wrapped ? output.output : wrappedText;
  let exitCode =
    native && Number.isInteger(output.exitCode)
      ? output.exitCode
      : wrapped && Number.isInteger(output.exit_code)
        ? output.exit_code
        : null;
  let durationMs =
    native && Number.isFinite(output.durationMs)
      ? output.durationMs
      : wrapped && Number.isFinite(output.wall_time_seconds)
        ? Math.round(output.wall_time_seconds * 1000)
        : null;
  let event = null;
  if (!native && !wrapped && typeof text === "string") {
    // Codex tool wrappers add this envelope around a command's combined output.
    // Parse it only when the complete prefix is present, so ordinary output is preserved.
    const match = text.match(
      /^(?:Chunk ID: [^\n]+\n)?Wall time: ([\d.]+) seconds\n(Process exited with code (-?\d+)|Process running with session ID \d+|Script running with cell ID [^\n]+)\n(?:Final output:|Output:)\n/,
    );
    if (match) {
      durationMs = Math.round(Number(match[1]) * 1000);
      exitCode = match[3] === undefined ? null : Number(match[3]);
      event = exitCode === null ? "running" : "completed";
      text = text.slice(match[0].length);
    }
  }
  const raw = typeof text === "string" ? text : "";
  const parsed = parsedJson(raw);
  const extra =
    native || wrapped
      ? Object.fromEntries(
          Object.entries(output).filter(
            ([key]) =>
              ![
                "aggregatedOutput",
                "exitCode",
                "durationMs",
                "output",
                "exit_code",
                "wall_time_seconds",
              ].includes(key),
          ),
        )
      : text === null
        ? output
        : null;
  return {
    text: raw,
    exitCode,
    durationMs,
    event: event || status,
    parsed,
    extra: extra && (typeof extra !== "object" || Object.keys(extra).length) ? extra : null,
  };
}

export function appendLiveCommandText(previous, delta) {
  const next = previous + delta;
  return next.length > MAX_LIVE_CHARS ? next.slice(-MAX_LIVE_CHARS) : next;
}
