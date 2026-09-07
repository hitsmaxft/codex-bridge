const encoder = new TextEncoder();
const decoder = new TextDecoder();

let instancePromise;

async function instantiateDemo() {
  const url = new URL("codex_bridge_demo.wasm", document.baseURI);
  const response = await fetch(url);
  if (!response.ok) throw new Error(`demo_wasm_unavailable: ${response.status}`);
  if (WebAssembly.instantiateStreaming) {
    try {
      return (await WebAssembly.instantiateStreaming(response.clone(), {})).instance;
    } catch {
      // Static hosts with an incorrect MIME type still work through the byte fallback.
    }
  }
  return (await WebAssembly.instantiate(await response.arrayBuffer(), {})).instance;
}

function demoInstance() {
  instancePromise ||= instantiateDemo();
  return instancePromise;
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

export function demoCommandWithInstance(instance, request) {
  const bytes = encoder.encode(JSON.stringify(request));
  const requestPointer = instance.exports.demo_alloc(bytes.length);
  new Uint8Array(instance.exports.memory.buffer, requestPointer, bytes.length).set(bytes);
  let packed;
  try {
    packed = instance.exports.demo_command(requestPointer, bytes.length);
  } finally {
    instance.exports.demo_free(requestPointer, bytes.length);
  }
  const responsePointer = Number(packed & 0xffff_ffffn);
  const responseLength = Number(packed >> 32n);
  const responseBytes = new Uint8Array(
    instance.exports.memory.buffer,
    responsePointer,
    responseLength,
  ).slice();
  instance.exports.demo_free(responsePointer, responseLength);

  return JSON.parse(decoder.decode(responseBytes));
}

export async function demoCommand(request) {
  const response = demoCommandWithInstance(await demoInstance(), request);

  // Keep the real UI's submitting state visible long enough to inspect in the public demo.
  if (["send", "steer"].includes(request.command)) await delay(420);
  return response;
}
