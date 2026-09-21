import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import * as vscode from "vscode";
import type { OpenedElf, SymbolNode } from "../../src/elf/api";
import type { ConnectRequest, SessionEvent } from "../../src/live/api";

import { launchDefaults } from "./launch";
import { ProbeGuard, ProbeHolder } from "./probeGuard";
import { findServer } from "./serverPath";
import { StudioServer } from "./server";

export const STORAGE_KEY = "tuningStudio.storage";
export const LAST_ELF_KEY = "tuningStudio.lastElf";
/** Where recordings go, under the first workspace folder */
const RECORDINGS_DIR = [".tuning-studio", "recordings"];

const DEBUGGER_UNSUPPORTED =
  "Sharing the probe with a debug session is not supported: each read through the debugger takes " +
  "about 14 ms and probe-rs halts the core around it. Stop the debug session to use the probe here, or connect over USB.";

interface Notice {
  id: number;
  message: string;
  reconnect: boolean;
}


/** Messages from the page */
export type FromPage =
  | { type: "call"; id: number; method: string; params: Record<string, unknown> }
  | { type: "storage"; key: string; value: string };

/** Owns the process and probe independently of any editor or sidebar. */
export class StudioSession implements ProbeHolder {
  private server: StudioServer | null = null;
  private session: { id: number; carrier: string; live: boolean } | null = null;
  private request: ConnectRequest | null = null;
  private releasedFor: string | null = null;
  private notice: Notice | null = null;
  private nextNotice = 1;
  private readonly disposables: vscode.Disposable[] = [];
  private readonly messages = new vscode.EventEmitter<unknown>();
  readonly onMessage = this.messages.event;
  private readonly changes = new vscode.EventEmitter<void>();
  readonly onDidChange = this.changes.event;
  private readonly snapshots = new Map<string, SessionEvent>();
  private logLines: Extract<SessionEvent, { type: "log" }>["lines"] = [];
  elf: OpenedElf | null = null;
  state = "disconnected";
  get lastConnection(): ConnectRequest | null { return this.request ? { ...this.request } : null; }
  recording = false;
  private disposed = false;
  private savedValues: [number, number][] = [];
  private pendingWatches: SymbolNode[] = [];
  queueWatch(node: SymbolNode) {
    this.pendingWatches.push(node);
    this.postMessage({ type: "watches_pending" });
  }

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly guard: ProbeGuard,
    private readonly log: vscode.OutputChannel,
    readonly firmwareLog: vscode.OutputChannel,
  ) {
    this.disposables.push(guard.add(this), this.messages, this.changes);
  }

  private postMessage(message: unknown) { this.messages.fire(message); }

  get active() { return this.session?.live ?? false; }
  get targetLabel() {
    return this.request?.carrier === "serial" ? this.request.port ?? "USB target" : this.request?.chip || "Debug probe";
  }

  private changed() {
    void vscode.commands.executeCommand("setContext", "tuningStudio.connected", this.active);
    void vscode.commands.executeCommand("setContext", "tuningStudio.recording", this.recording);
    this.changes.fire();
  }

  /** Re-open views against the same session, without opening the probe again. */
  refreshPage() { this.postMessage({ type: "refresh" }); }

  // ProbeHolder

  holdsProbe(): boolean {
    return this.session?.carrier === "probe" && this.session.live;
  }

  async releaseProbe(name: string) {
    if (!this.server || !this.holdsProbe()) return;
    await this.server.call("session_disconnect");
    if (this.session) this.session.live = false;
    this.releasedFor = name;
    this.setNotice(`Released the probe to the debug session "${name}".`, false);
    void vscode.window.showInformationMessage(`Tuning Studio released the probe for the debug session "${name}".`);
  }

  debugSessionsChanged() {
    if (this.releasedFor && !this.guard.blockedBy()) {
      this.setNotice(`The debug session "${this.releasedFor}" ended; the probe is free.`, true);
      this.releasedFor = null;
    }
    this.postStatus();
  }

  // The page

  private status() {
    const blocker = this.guard.blockedBy();
    return {
      sources: {
        probe: blocker
          ? {
              available: false,
              reason: `The debug session "${blocker}" is using the probe. Stop it to connect through the probe here, or connect over USB.`,
            }
          : { available: true, reason: null },
        serial: { available: true, reason: null },
        debugger: { available: false, reason: DEBUGGER_UNSUPPORTED },
      },
      notice: this.notice,
      features: {
        record: { available: true, reason: null },
        exportCsv: { available: true, reason: null },
        stream: { available: true, reason: null },
        reveal: { available: true, reason: null },
      },
    };
  }

  private setNotice(message: string, reconnect: boolean) {
    this.notice = { id: this.nextNotice++, message, reconnect };
    this.postStatus();
  }

  private postStatus() {
    void this.postMessage({ type: "status", status: this.status() });
  }

  async receive(m: FromPage, reply: (message: unknown) => void) {
    if (m.type === "storage") {
      const stored = this.context.workspaceState.get<Record<string, string>>(STORAGE_KEY) ?? {};
      stored[m.key] = m.value;
      await this.context.workspaceState.update(STORAGE_KEY, stored);
      return;
    }
    if (m.type !== "call") return;
    try {
      const result = await this.call(m.method, m.params ?? {});
      void reply({ type: "result", id: m.id, ok: true, result: result ?? null });
    } catch (e) {
      const error = e instanceof Error ? e.message : String(e);
      void reply({ type: "result", id: m.id, ok: false, error });
    }
  }

  async call(method: string, params: Record<string, unknown>): Promise<unknown> {
    switch (method) {
      case "take_pending_watches": {
        const nodes = this.pendingWatches;
        this.pendingWatches = [];
        return nodes;
      }
      case "workbench_command": {
        if (!["tuningStudio.connect", "tuningStudio.disconnect", "tuningStudio.openElf", "tuningStudio.showLogs", "tuningStudio.target.focus"].includes(String(params.command))) {
          throw new Error("Unknown workbench command");
        }
        return vscode.commands.executeCommand(String(params.command));
      }
      case "host_status":
        return this.status();
      case "startup":
        return this.startup();
      case "pick_elf":
        return this.pickElf();
      case "session_attach": {
        if (!this.active || params.session !== this.session?.id) throw new Error("The target session ended. Connect again.");
        return [...this.snapshots.values()].map(event => event.type === "tune" ? { ...event, savedValues: this.savedValues } : event).concat({ type: "log", lines: this.logLines });
      }
      case "session_set_rate": {
        const result = await this.serverCall(method, params);
        if (this.request) this.request.rateHz = Number(params.hz);
        return result;
      }
      case "session_save": {
        const result = await this.serverCall(method, params);
        const tune = this.snapshots.get("tune");
        if (tune?.type === "tune") this.savedValues = tune.values.flatMap(v => v.requested === null ? [] : [[v.id, v.requested] as [number, number]]);
        return result;
      }
      case "session_disconnect": {
        const result = await this.serverCall(method, params);
        if (this.session) this.session.live = false;
        this.state = "disconnected";
        this.changed();
        return result;
      }
      case "session_connect":
        return this.connect(params);
      case "recording_start":
        return this.serverCall(method, { ...params, dir: this.recordingsDir() });
      case "pick_recording_path":
        return this.pickSave("Record to", "Record", this.recordingsDir(), { "MCAP recording": ["mcap"] });
      case "pick_csv_path":
        return this.pickSave("Export CSV", "Export", String(params.suggested ?? ""), { CSV: ["csv"] });
      case "pick_recording":
        return this.pickRecording();
      case "reveal":
        await vscode.commands.executeCommand("revealFileInOS", vscode.Uri.file(String(params.path)));
        return null;
      case "open_elf": {
        // A reattached scope must not reset the active backend or its watches.
        if (this.elf?.summary.path === params.path && !params.reload) return this.elf;
        if (this.active) throw new Error("Disconnect the target before changing firmware.");
        const result = await this.serverCall(method, params) as OpenedElf;
        this.elf = result;
        this.pendingWatches = [];
        await this.context.workspaceState.update(LAST_ELF_KEY, params.path);
        this.changed();
        return result;
      }
      default:
        return this.serverCall(method, params);
    }
  }

  startup() {
    const launch = launchDefaults();
    const last = this.context.workspaceState.get<string>(LAST_ELF_KEY);
    const elfPath = this.active ? this.elf?.summary.path ?? null : this.elf?.summary.path ?? (last && existsSync(last) ? last : (launch?.elfPath ?? null));
    if (launch) this.log.appendLine(`launch configuration "${launch.name}": chip ${launch.chip}, ELF ${launch.elfPath}`);
    return {
      elfPath,
      connect: this.active ? this.request : null,
      resumeSession: this.active ? this.session?.id : undefined,
      connectDefaults: launch
        ? { chip: launch.chip ?? undefined, probe: launch.probe ?? undefined, speedKhz: launch.speedKhz ?? undefined }
        : null,
      watches: [],
    };
  }

  async pickElf(): Promise<string | null> {
    const near = this.context.workspaceState.get<string>(LAST_ELF_KEY) ?? launchDefaults()?.elfPath;
    const picked = await vscode.window.showOpenDialog({
      title: "Open firmware ELF",
      openLabel: "Open ELF",
      canSelectMany: false,
      canSelectFolders: false,
      defaultUri: near ? vscode.Uri.file(dirname(near)) : vscode.workspace.workspaceFolders?.[0]?.uri,
    });
    return picked?.[0]?.fsPath ?? null;
  }

  /** `<workspace>/.tuning-studio/recordings`; null outside a workspace, so the server picks */
  private recordingsDir(): string | null {
    const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    return folder ? join(folder, ...RECORDINGS_DIR) : null;
  }

  private async pickSave(
    title: string,
    saveLabel: string,
    near: string | null,
    filters: Record<string, string[]>,
  ): Promise<string | null> {
    const picked = await vscode.window.showSaveDialog({
      title,
      saveLabel,
      filters,
      defaultUri: near ? vscode.Uri.file(near) : vscode.workspace.workspaceFolders?.[0]?.uri,
    });
    return picked?.fsPath ?? null;
  }

  private async pickRecording(): Promise<string | null> {
    const dir = this.recordingsDir();
    const picked = await vscode.window.showOpenDialog({
      title: "Export a recording as CSV",
      openLabel: "Export",
      canSelectMany: false,
      canSelectFolders: false,
      filters: { "MCAP recording": ["mcap"] },
      defaultUri: dir && existsSync(dir) ? vscode.Uri.file(dir) : vscode.workspace.workspaceFolders?.[0]?.uri,
    });
    return picked?.[0]?.fsPath ?? null;
  }

  private async connect(params: Record<string, unknown>) {
    const request = params.request as unknown as ConnectRequest;
    if (this.active) throw new Error("Disconnect the current target before connecting again.");
    const blocker = this.guard.blockedBy();
    if (request.carrier === "probe" && blocker) {
      throw new Error(`The debug session "${blocker}" is using the probe; stop it first, or connect over USB.`);
    }
    this.snapshots.clear();
    this.logLines = [];
    this.savedValues = [];
    this.request = request;
    this.state = "connecting";
    this.session = { id: params.session as number, carrier: request.carrier, live: true };
    this.changed();
    this.releasedFor = null;
    if (this.notice) {
      this.notice = null;
      this.postStatus();
    }
    try {
      return await this.serverCall("session_connect", params);
    } catch (e) {
      if (this.session?.id === params.session) this.session.live = false;
      this.state = "failed";
      this.changed();
      throw e;
    }
  }

  // studio-server

  private startServer(): StudioServer {
    const path = findServer(this.context.extensionPath);
    if (!path) {
      throw new Error(
        "studio-server not found. Build it (cargo build -p studio-server) or set tuningStudio.serverPath.",
      );
    }
    const args = vscode.workspace.getConfiguration("tuningStudio").get<boolean>("mockTarget") ? ["--mock"] : [];
    this.log.appendLine(`starting ${path} ${args.join(" ")}`);
    const server = new StudioServer(path, args, {
      event: (session, event) => {
        if (this.session?.id !== session) return;
        const typed = event as SessionEvent;
        if (typed.type === "log") {
          this.logLines = this.logLines.concat(typed.lines).slice(-1000);
          for (const line of typed.lines) this.firmwareLog.appendLine(
            `${line.timestamp ?? ""} ${line.level ?? ""} ${line.message}${line.location ? ` (${line.location})` : ""}`.trim(),
          );
        } else {
          if (typed.type === "tune" && this.savedValues.length === 0) {
            this.savedValues = typed.values.flatMap(v => v.requested === null ? [] : [[v.id, v.requested] as [number, number]]);
          }
          this.snapshots.set(typed.type, typed);
        }
        if (typed.type === "status") {
          this.session.live = typed.state === "connecting" || typed.state === "connected";
          this.state = typed.state;
          this.changed();
        }
        void this.postMessage({ type: "event", session, event });
      },
      appEvent: (event) => {
        if (event.type === "recording" && this.recording !== (event.active === true)) {
          this.recording = event.active === true;
          this.changed();
        }
        void this.postMessage({ type: "app_event", event });
      },
      frame: (session, tts1) => {
        void this.postMessage({ type: "frame", session, data: tts1 });
      },
      exit: (code, expected) => {
        if (this.server === server) this.server = null;
        if (expected) return;
        this.log.appendLine(`studio-server exited (code ${code})`);
        void vscode.window.showErrorMessage(`Tuning Studio: studio-server stopped (code ${code}). Reopen the ELF to go on.`);
        if (this.session) {
          const event = { type: "status", state: "failed", message: "studio-server stopped" };
          void this.postMessage({ type: "event", session: this.session.id, event });
          this.session.live = false;
          this.snapshots.clear();
          this.state = "failed";
        }
        this.elf = null;
        this.recording = false;
        this.changed();
      },
      log: (line) => this.log.appendLine(line),
    });
    server.ready.then((r) => this.log.appendLine(`studio-server ${r.version} ready${r.mock ? " (mock target)" : ""}`), () => {});
    this.server = server;
    return server;
  }

  private serverCall(method: string, params: Record<string, unknown>) {
    if (this.disposed) return Promise.reject(new Error("Tuning Studio has stopped."));
    return (this.server ?? this.startServer()).call(method, params);
  }

  dispose() {
    this.disposed = true;
    this.disposables.forEach((d) => d.dispose());
    void this.server?.dispose();
    this.server = null;
  }
}
