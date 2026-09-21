import { basename, isAbsolute, resolve } from "node:path";
import * as vscode from "vscode";
import type { Children, SymbolNode } from "../../src/elf/api";
import type { ConnectRequest, PortInfo, ProbeInfo } from "../../src/live/api";
import { StudioSession } from "./session";

type SymbolEntry = SymbolNode | { parent: SymbolNode; offset: number; total: number };

interface TargetRow { label: string; description?: string; command: string; icon: string }

/** Real workbench trees: keyboard navigation, context menus and movable views. */
export function registerWorkbench(context: vscode.ExtensionContext, session: StudioSession, showScope: () => void) {
  const targetChanges = new vscode.EventEmitter<void>();
  const symbolChanges = new vscode.EventEmitter<void>();
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 30);
  status.name = "Tuning Studio target";
  status.command = "tuningStudio.targetActions";

  const refresh = () => {
    targetChanges.fire();
    status.text = `$(pulse) ${session.active ? session.targetLabel : "Tuning Studio"}: ${session.state}${session.recording ? " $(record)" : ""}`;
    status.tooltip = "Target actions • The target stays connected when the scope closes";
    status.show();
  };
  let lastElf = session.elf;
  context.subscriptions.push(targetChanges, symbolChanges, status,
    session.onDidChange(() => {
      refresh();
      if (lastElf !== session.elf) { lastElf = session.elf; symbolChanges.fire(); }
    }),
    vscode.window.registerTreeDataProvider<TargetRow>("tuningStudio.target", {
      onDidChangeTreeData: targetChanges.event,
      getChildren: () => [
        { label: session.active ? session.targetLabel : "Connect target", description: session.state,
          command: "tuningStudio.targetActions", icon: "plug" },
        { label: session.elf ? basename(session.elf.summary.path) : "Open firmware ELF…",
          description: session.elf?.summary.machine, command: "tuningStudio.openElf", icon: "file-binary" },
        { label: "Open scope", command: "tuningStudio.open", icon: "graph-line" },
        ...(session.recording ? [{ label: "Stop recording", command: "tuningStudio.stopRecording", icon: "record" }] : []),
        { label: "Firmware log", command: "tuningStudio.showLogs", icon: "output" },
      ],
      getTreeItem: (row) => {
        const item = new vscode.TreeItem(row.label);
        item.description = row.description;
        item.iconPath = new vscode.ThemeIcon(row.icon);
        item.command = { command: row.command, title: row.label };
        return item;
      },
    }),
    vscode.window.registerTreeDataProvider<SymbolEntry>("tuningStudio.symbols", {
      onDidChangeTreeData: symbolChanges.event,
      async getChildren(node) {
        if (!node) return session.elf?.roots ?? [];
        const parent = "parent" in node ? node.parent : node;
        const offset = "parent" in node ? node.offset : 0;
        const result = await session.call("symbol_children", { node: parent.ref, limit: offset + 200 }) as Children;
        const entries: SymbolEntry[] = result.nodes.slice(offset);
        if (result.total > result.nodes.length) entries.push({ parent, offset: result.nodes.length, total: result.total });
        return entries;
      },
      getTreeItem(node) {
        if ("parent" in node) {
          const more = new vscode.TreeItem(`More elements (${node.offset} of ${node.total})`, vscode.TreeItemCollapsibleState.Collapsed);
          more.id = `${JSON.stringify(node.parent.ref)}:more:${node.offset}`;
          return more;
        }
        const item = new vscode.TreeItem(node.label, node.expandable ? vscode.TreeItemCollapsibleState.Collapsed : vscode.TreeItemCollapsibleState.None);
        item.id = JSON.stringify(node.ref);
        item.description = node.typeName;
        item.tooltip = `${node.path}\n${node.typeName}\n0x${node.address.toString(16)}`;
        item.contextValue = node.location ? "studioSymbolSource" : "studioSymbol";
        item.iconPath = new vscode.ThemeIcon(node.expandable ? "symbol-struct" : "symbol-variable");
        return item;
      },
    }),
  );

  // All commands share one error boundary; cancelled pickers simply return.
  const command = (name: string, fn: (...args: any[]) => unknown) => context.subscriptions.push(
    vscode.commands.registerCommand(`tuningStudio.${name}`, async (...args: any[]) => {
      try { return await fn(...args); }
      catch (e) { void vscode.window.showErrorMessage(`Tuning Studio: ${e instanceof Error ? e.message : e}`); }
    }),
  );

  command("open", showScope);
  command("openElf", async (uri?: vscode.Uri) => {
    if (session.active) throw new Error("Disconnect the target before changing firmware.");
    const path = uri?.fsPath ?? await session.pickElf();
    if (!path) return;
    // Explicitly reopening a file also reloads a rebuilt ELF.
    await session.call("open_elf", { path, reload: true });
    session.refreshPage();
    await vscode.commands.executeCommand("tuningStudio.symbols.focus");
  });
  command("connect", async () => {
    if (session.active) return;
    const transport = await vscode.window.showQuickPick([
      { label: "Debug probe", description: "SWD • firmware ELF required", carrier: "probe" as const },
      { label: "USB", description: "Firmware tuning link • no ELF required", carrier: "serial" as const },
    ], { title: "Connect target", placeHolder: "Choose a connection" });
    if (!transport) return;
    const defaults = session.startup();
    const request: ConnectRequest = { carrier: transport.carrier, chip: defaults.connectDefaults?.chip ?? "", probe: null, port: null,
      speedKhz: defaults.connectDefaults?.speedKhz ?? vscode.workspace.getConfiguration("tuningStudio").get<number>("swdSpeedKhz", 4000), rateHz: vscode.workspace.getConfiguration("tuningStudio").get<number>("sampleRateHz", 100) };
    if (transport.carrier === "probe") {
      if (!session.elf) {
        const path = defaults.elfPath ?? await session.pickElf();
        if (!path) return;
        await session.call("open_elf", { path });
        session.refreshPage();
      }
      const probes = await session.call("list_probes", {}) as ProbeInfo[];
      const selected = await vscode.window.showQuickPick(probes.map(p => ({ label: p.name, description: p.serial ?? p.selector, probe: p.selector })),
        { title: "Debug probe", placeHolder: probes.length ? "Select a probe" : "No probes found — attach one and try again" });
      if (!selected) return;
      request.probe = selected.probe;
      const chip = await vscode.window.showInputBox({ title: "Target chip", value: request.chip, prompt: "probe-rs target name, e.g. STM32H723VGTx",
        validateInput: value => value.trim() ? undefined : "Enter a target chip" });
      if (chip === undefined) return;
      request.chip = chip.trim();
    } else {
      const ports = await session.call("list_serial_ports", {}) as PortInfo[];
      const selected = await vscode.window.showQuickPick(ports.map(p => ({ label: p.product ?? p.path, description: p.path, port: p.path })),
        { title: "USB target", placeHolder: ports.length ? "Select a serial port" : "No ports found — attach the target and try again" });
      if (!selected) return;
      request.port = selected.port;
    }
    await session.call("session_connect", { request, session: Math.floor(Math.random() * 0x3fffffff) + 1 });
    session.refreshPage();
  });
  command("setRate", async () => {
    if (!session.active) throw new Error("Connect a target before changing the sample rate.");
    const rate = await vscode.window.showQuickPick([10, 50, 100, 200, 500, 1000].map(hz => ({ label: `${hz} Hz`, hz })), { title: "Sample rate" });
    if (rate) await session.call("session_set_rate", { hz: rate.hz });
  });
  command("disconnect", () => session.call("session_disconnect", {}));
  command("startRecording", async () => {
    if (!session.active) throw new Error("Connect a target before recording.");
    await session.call("recording_start", { path: null });
  });
  command("stopRecording", () => session.call("recording_stop", {}));
  command("showLogs", () => session.firmwareLog.show(true));
  command("showSource", async (node: SymbolNode) => {
    if (!node?.location) return;
    const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!isAbsolute(node.location.file) && !folder) throw new Error("Open the firmware workspace to resolve this source path.");
    const path = isAbsolute(node.location.file) ? node.location.file : resolve(folder!, node.location.file);
    const document = await vscode.workspace.openTextDocument(vscode.Uri.file(path));
    const editor = await vscode.window.showTextDocument(document, { preview: true });
    const position = new vscode.Position(Math.max(0, node.location.line - 1), 0);
    editor.selection = new vscode.Selection(position, position);
    editor.revealRange(new vscode.Range(position, position));
  });
  command("plotSymbol", async (node: SymbolNode) => {
    if (!node?.ref) return;
    // Queue in the session so this also works when the scope has never been opened.
    session.queueWatch(node);
    showScope();
  });
  command("targetActions", async () => {
    const picked = await vscode.window.showQuickPick([
      { label: session.active ? "Disconnect target" : "Connect target…", action: session.active ? "disconnect" : "connect" },
      { label: "Open scope", action: "open" },
      ...(session.active ? [{ label: "Change sample rate…", action: "setRate" }] : []),
      ...(!session.active ? [{ label: "Open firmware ELF…", action: "openElf" }] : []),
      ...(session.active ? [{ label: session.recording ? "Stop recording" : "Start recording", action: session.recording ? "stopRecording" : "startRecording" }] : []),
      { label: "Show firmware log", action: "showLogs" },
    ], { title: "Tuning Studio" });
    if (picked) await vscode.commands.executeCommand(`tuningStudio.${picked.action}`);
  });
  refresh();
}
