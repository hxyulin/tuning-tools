// Inside a VS Code webview: every call is a message to the extension, which answers it itself
// (dialogs, launch configurations) or passes it to studio-server. Method names and arguments are
// the Tauri commands', so the two backends stay one API.

import type { SymbolNode } from "../elf/api";
import type { ConnectRequest, SessionEvent } from "../live/api";
import type { AppEvent } from "../live/recording";
import { STANDALONE_STATUS, appEventHub } from "./common";
import type { Host, HostStartup, HostStatus, HostStorage, SessionHandlers } from "./types";

interface VsCodeApi {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
}

declare global {
  function acquireVsCodeApi(): VsCodeApi;
}

/** Messages from the extension */
type Incoming =
  | { type: "refresh" }
  | { type: "watches_pending" }
  | { type: "result"; id: number; ok: true; result: unknown }
  | { type: "result"; id: number; ok: false; error: string }
  | { type: "event"; session: number; event: SessionEvent }
  | { type: "frame"; session: number; data: ArrayBuffer | Uint8Array }
  | { type: "status"; status: HostStatus }
  | { type: "app_event"; event: AppEvent };

interface WebviewState {
  storage: Record<string, string>;
}

export function inVsCode(): boolean {
  return typeof acquireVsCodeApi === "function";
}

/** Storage the extension put in the page, for a panel opened afresh */
function bootStorage(): Record<string, string> {
  try {
    const boot = document.getElementById("tuning-studio-boot")?.textContent;
    return boot ? (JSON.parse(boot).storage ?? {}) : {};
  } catch {
    return {};
  }
}

/** The frame as an 8-byte aligned buffer of its own, for the Float64Array views */
function ownBuffer(data: ArrayBuffer | Uint8Array): ArrayBuffer {
  if (data instanceof ArrayBuffer) return data;
  if (data.byteOffset % 8 === 0 && data.byteLength === data.buffer.byteLength) return data.buffer as ArrayBuffer;
  return data.slice().buffer as ArrayBuffer;
}

/** Follow the editor's light or dark theme, which VS Code marks with a class on the body */
function followTheme() {
  const apply = () => {
    const c = document.body.classList;
    const light = c.contains("vscode-light") || c.contains("vscode-high-contrast-light");
    document.documentElement.dataset.theme = light ? "light" : "dark";
  };
  apply();
  new MutationObserver(apply).observe(document.body, { attributes: true, attributeFilter: ["class"] });
}

export function createVsCodeHost(): Host {
  followTheme();
  const vscode = acquireVsCodeApi();
  const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  let nextCall = 1;
  let resumeSession: number | undefined;
  let watchListener: ((nodes: SymbolNode[]) => void) | undefined;
  const takeWatches = () => {
    if (!watchListener) return;
    void call<SymbolNode[]>("take_pending_watches").then(nodes => watchListener?.(nodes), () => {});
  };
  // Fresh connections use a new ID; startup explicitly identifies a session to resume.
  let nextSession = Math.floor(Math.random() * 0x3fffffff) + 1;
  /** Only the latest connect's handlers get output, like a fresh Tauri channel per connect */
  let session: { id: number; handlers: SessionHandlers } | null = null;
  let status: HostStatus = STANDALONE_STATUS;
  const listeners = new Set<(s: HostStatus) => void>();
  const appEvents = appEventHub();
  const startupListeners = new Set<() => void>();

  // Webview state survives the panel being hidden; the extension's workspace state survives a restart
  const saved = vscode.getState() as WebviewState | undefined;
  const store: Record<string, string> = { ...bootStorage(), ...(saved?.storage ?? {}) };
  const storage: HostStorage = {
    get: (key) => store[key] ?? null,
    set(key, value) {
      store[key] = value;
      vscode.setState({ storage: store } satisfies WebviewState);
      vscode.postMessage({ type: "storage", key, value });
    },
  };

  window.addEventListener("message", (e: MessageEvent<Incoming>) => {
    const m = e.data;
    switch (m?.type) {
      case "refresh":
        startupListeners.forEach((listener) => listener());
        break;
      case "watches_pending":
        takeWatches();
        break;
      case "result": {
        const call = pending.get(m.id);
        if (!call) return;
        pending.delete(m.id);
        if (m.ok) call.resolve(m.result);
        else call.reject(new Error(m.error));
        break;
      }
      case "event":
        if (session?.id === m.session) session.handlers.event(m.event);
        break;
      case "frame":
        if (session?.id === m.session) session.handlers.frame(ownBuffer(m.data));
        break;
      case "status":
        status = m.status;
        listeners.forEach((l) => l(status));
        break;
      case "app_event":
        appEvents.emit(m.event);
        break;
    }
  });

  function call<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    const id = nextCall++;
    return new Promise<T>((resolve, reject) => {
      pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      vscode.postMessage({ type: "call", id, method, params });
    });
  }

  // The first status comes as an answer, so a listener never waits for a change
  call<HostStatus>("host_status").then((s) => {
    status = s;
    listeners.forEach((l) => l(status));
  }, () => {});

  return {
    name: "vscode",
    storage,
    startup: async () => {
      const startup = await call<HostStartup>("startup");
      resumeSession = startup.resumeSession;
      return startup;
    },
    watchStartup(listener) {
      startupListeners.add(listener);
      return () => { startupListeners.delete(listener); };
    },
    workbenchCommand: (command) => call("workbench_command", { command }),
    watchRequests(listener) {
      watchListener = listener;
      takeWatches();
      return () => { watchListener = undefined; };
    },
    watchStatus(listener) {
      listeners.add(listener);
      listener(status);
      return () => listeners.delete(listener);
    },
    pickElf: () => call<string | null>("pick_elf"),
    openElf: (path) => call("open_elf", { path }),
    symbolChildren: (node, limit) => call("symbol_children", { node, limit: limit ?? null }),
    inspectChildren: (node, offset, limit, live) => call("inspect_children", {node, offset, limit, live}),
    nodeMetadata: (node) => call("node_metadata", {node}),
    watchableLeaves: (node) => call("watchable_leaves", { node }),
    listProbes: () => call("list_probes"),
    listSerialPorts: () => call("list_serial_ports"),
    searchChips: (query) => call("search_chips", { query }),
    async connect(request: ConnectRequest, handlers) {
      const resume = resumeSession;
      resumeSession = undefined;
      const id = resume ?? nextSession++;
      session = { id, handlers };
      if (resume !== undefined) {
        const events = await call<SessionEvent[]>("session_attach", { session: id });
        if (session?.id === id) events.forEach(event => handlers.event(event));
      } else {
        await call("session_connect", { request, session: id });
      }
    },
    disconnect: () => call("session_disconnect"),
    setRate: (hz) => call("session_set_rate", { hz }),
    setWatches: (watches) => call("session_set_watches", { watches }),
    requestValue: (id, value) => call("session_request", { id, value }),
    saveValues: () => call("session_save"),
    discardValues: () => call("session_discard"),
    taskTrace: () => call("session_task_trace"),
    taskStates: () => call("session_task_states"),
    readValues: (nodes) => call("session_read_values", { nodes }),
    // The extension adds the workspace's recordings directory
    recordingStart: (path) => call("recording_start", { path }),
    recordingStop: () => call("recording_stop"),
    exportCsv: (mcapPath, csvPath) => call("export_csv", { mcapPath, csvPath }),
    streamStart: (port, bindAll) => call("stream_start", { port, bindAll }),
    streamStop: () => call("stream_stop"),
    appState: () => call("app_state"),
    watchAppEvents: appEvents.watch,
    pickRecordingPath: () => call<string | null>("pick_recording_path"),
    pickCsvPath: (suggested) => call<string | null>("pick_csv_path", { suggested }),
    pickRecording: () => call<string | null>("pick_recording"),
    reveal: (path) => call("reveal", { path }),
  };
}
