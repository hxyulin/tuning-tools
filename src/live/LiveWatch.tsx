import { useEffect, useMemo, useState } from "react";
import { RootNode, SymbolNode } from "../elf/api";
import { RowNote, SymbolTree } from "../elf/SymbolTree";
import { host } from "../host";
import { button } from "../ui";
import { Carrier, ValueRead } from "./api";
import { formatValue } from "./format";

const LIMIT = 128;
const INTERVAL_MS = 200;

/** Low-rate inspection shares the session's probe; it never adds plot subscriptions. */
export function LiveWatch({ roots, connected, carrier, onWatch, watched }: {
  roots: RootNode[];
  connected: boolean;
  carrier: Carrier | null;
  onWatch: (node: SymbolNode) => void;
  watched: Set<string>;
}) {
  const [selected, setSelected] = useState<SymbolNode | null>(null);
  const [visible, setVisible] = useState<SymbolNode[]>([]);
  const [paused, setPaused] = useState(false);
  const [pageVisible, setPageVisible] = useState(!document.hidden);
  const [values, setValues] = useState<Map<string, ValueRead>>(() => new Map());
  const [error, setError] = useState<string | null>(null);
  const nodes = useMemo(() => visible.slice(0, LIMIT), [visible]);
  const live = connected && carrier === "probe";

  useEffect(() => {
    const change = () => setPageVisible(!document.hidden);
    document.addEventListener("visibilitychange", change);
    return () => document.removeEventListener("visibilitychange", change);
  }, []);

  useEffect(() => {
    setSelected(null);
    setVisible([]);
  }, [roots]);

  useEffect(() => {
    if (!live || !nodes.length) setValues(new Map());
    if (!live || paused || !pageVisible || !nodes.length) return;
    setValues(new Map());
    setError(null);
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      try {
        const result = await host.readValues(nodes.map((node) => node.ref));
        if (stopped) return;
        setValues(new Map(nodes.map((node, i) => [node.path, result[i] ?? { value: null, error: "No reading returned" }])));
        setError(null);
      } catch (e) {
        if (stopped) return;
        setValues(new Map());
        setError(String(e));
      } finally {
        // Schedule after completion: slow probes never accumulate pending polls.
        if (!stopped) timer = setTimeout(poll, INTERVAL_MS);
      }
    };
    void poll();
    return () => { stopped = true; clearTimeout(timer); };
  }, [nodes, roots, live, paused, pageVisible]);

  const notes = useMemo(() => new Map<string, RowNote>(visible.map((node, i) => {
    const read = values.get(node.path);
    return [node.path, {
      text: i >= LIMIT ? "limit reached" : read?.error ? "read failed" : read?.value == null ? "—" : formatValue(read.value, node.scalar),
      title: read?.error ?? `${node.typeName} · ${node.path}`,
      tone: read?.error ? "danger" : live && !paused ? "ink" : "muted",
    }];
  })), [visible, values, live, paused]);

  return <div className="flex h-full min-h-0 flex-col">
    <div className="flex flex-wrap items-center gap-2 border-b border-rule px-3 py-2 text-[12px]">
      <span className="flex-1 text-muted">{!connected ? "Connect a debug probe to read live values." : carrier !== "probe" ? "Live Watch needs SWD; USB values are available in Tune." : paused ? "Paused" : `${nodes.length} fields · up to 5 Hz`}</span>
      <button className={button} disabled={!live} aria-pressed={paused} onClick={() => setPaused((p) => !p)}>{paused ? "Resume" : "Pause"}</button>
    </div>
    {error && <p role="alert" className="px-3 py-2 text-[12px] text-danger">{error}</p>}
    {visible.length > LIMIT && <p role="status" className="px-3 py-1 text-[12px] text-warn">Showing the first {LIMIT} fields. Collapse a group or filter to inspect others.</p>}
    <div className="min-h-0 flex-1">
      <SymbolTree roots={roots} selected={selected} onSelect={setSelected} onWatch={onWatch} watched={watched} notes={notes} onVisibleNodes={setVisible} label="Live Watch" />
    </div>
    <p className="border-t border-rule px-3 py-2 text-[11px] text-muted">Expand structs and arrays to read their fields. Select a number and press W to plot it. Reads do not halt the target.</p>
  </div>;
}
