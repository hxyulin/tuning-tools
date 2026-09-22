import { useEffect, useMemo, useRef, useState } from "react";
import { NodeRef, RootNode, SymbolNode } from "../elf/api";
import { RowNote, SymbolTree } from "../elf/SymbolTree";
import { host } from "../host";
import { button, field } from "../ui";
import { Carrier, ValueRead } from "./api";
import { formatValue } from "./format";

const LIMIT = 128;
const INTERVAL_MS = 200;
type Pin = { ref: NodeRef; path: string };
type Groups = Record<string, Pin[]>;
const GROUP_KEY = "liveWatch.groups.v1";
function savedGroups(): Groups {
  try {
    const parsed = JSON.parse(host.storage.get(GROUP_KEY) ?? "{}");
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    return Object.fromEntries(Object.entries(parsed).filter(([, pins]) => Array.isArray(pins)).map(([name, pins]) => [name, (pins as Pin[]).filter((p) => p && typeof p.path === "string" && typeof p.ref?.symbol === "string" && Array.isArray(p.ref.steps)).slice(0, 128)]));
  } catch { return {}; }
}

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
  const [groups, setGroups] = useState<Groups>(savedGroups);
  const [group, setGroup] = useState("Watch");
  const [newGroup, setNewGroup] = useState("");
  const [pinnedOnly, setPinnedOnly] = useState(false);
  const [pinnedRoots, setPinnedRoots] = useState<RootNode[]>([]);
  const [pinError, setPinError] = useState<string | null>(null);
  const previous = useRef<Map<string, ValueRead>>(new Map());
  const changed = useRef<Map<string, number>>(new Map());
  useEffect(() => { try { host.storage.set(GROUP_KEY, JSON.stringify(groups)); } catch { setPinError("Could not save watch groups."); } }, [groups]);
  useEffect(() => {
    let cancelled = false;
    setPinError(null);
    Promise.allSettled((groups[group] ?? []).map(async (pin) => {
      const node = await host.nodeMetadata(pin.ref);
      const prefix = node.ref.symbol.lastIndexOf("::");
      return {...node, label: node.path.slice(prefix < 0 ? 0 : prefix + 2), segments: [node.path], section: "", readOnly: false, internal: false};
    })).then((results) => {
      if (cancelled) return;
      setPinnedRoots(results.flatMap((r) => r.status === "fulfilled" ? [r.value] : []));
      const missing = results.filter((r) => r.status === "rejected").length;
      if (missing) setPinError(`${missing} pinned symbols are unavailable in this ELF. Pins are retained for their original firmware.`);
    });
    return () => { cancelled = true; };
  }, [roots, groups, group]);
  const pin = () => {
    if (!selected) return;
    // Avoid displaying a node twice through a pinned ancestor and its child.
    const nested = (a: NodeRef, b: NodeRef) => a.symbol === b.symbol && a.steps.length <= b.steps.length && a.steps.every((step, i) => JSON.stringify(step) === JSON.stringify(b.steps[i]));
    setGroups((old) => ({...old, [group]: [...(old[group] ?? []).filter((p) => !nested(p.ref, selected.ref) && !nested(selected.ref, p.ref)), {ref: selected.ref, path: selected.path}].slice(-128)}));
  };
  const addGroup = () => { const name = newGroup.trim().slice(0, 40); if (name) { setGroups((old) => ({...old, [name]: Array.isArray(old[name]) ? old[name] : []})); setGroup(name); setNewGroup(""); } };
  const variantKey = JSON.stringify([...values].filter(([, v]) => v.activeVariant !== undefined).map(([path, v]) => [path, v.activeVariant]));
  const variants = useMemo(() => new Map<string, string | null>(JSON.parse(variantKey)), [variantKey]);
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
    previous.current.clear(); changed.current.clear();
    setValues(new Map());
  }, [roots]);

  useEffect(() => {
    if (!live || !nodes.length) setValues(new Map());
    if (!live || paused || !pageVisible || !nodes.length) return;
    setError(null);
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      try {
        const result = await host.readValues(nodes.map((node) => node.ref));
        if (stopped) return;
        const next = new Map(nodes.map((node, i) => [node.path, result[i] ?? { value: null, error: "No reading returned" }]));
        for (const [path, value] of next) {
          const before = previous.current.get(path);
          if (before && !before.error && !value.error && (before.value !== value.value || before.text !== value.text)) changed.current.set(path, Date.now() + 700);
        }
        previous.current = next;
        setValues(next);
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
      text: i >= LIMIT ? "limit reached" : read?.error ? "read failed" : read?.text ?? (read?.value == null ? "—" : node.kind === "pointer" ? `0x${read.value.toString(16)}` : formatValue(read.value, node.scalar)),
      title: read?.error ?? `${node.typeName} · ${node.path}`,
      tone: read?.error ? "danger" : live && !paused ? (changed.current.get(node.path) ?? 0) > Date.now() ? "accent" : "ink" : "muted",
    }];
  })), [visible, values, live, paused]);

  return <div className="flex h-full min-h-0 flex-col">
    <div className="flex flex-wrap items-center gap-2 border-b border-rule px-3 py-2 text-[12px]">
      <span className="flex-1 text-muted">{!connected ? "Connect a debug probe to read live values." : carrier !== "probe" ? "Live Watch needs SWD; USB values are available in Tune." : paused ? "Paused" : `${nodes.length} fields · up to 5 Hz`}</span>
      <button className={button} disabled={!live} aria-pressed={paused} onClick={() => setPaused((p) => !p)}>{paused ? "Resume" : "Pause"}</button>
    </div>
    <div className="flex flex-wrap items-center gap-1 border-b border-rule px-3 py-2 text-[11px]">
      <button className={button} aria-pressed={pinnedOnly} onClick={() => { setPinnedOnly((v) => !v); setSelected(null); }}>{pinnedOnly ? "Browse all" : "Pinned groups"}</button>
      <select className={`${field} min-w-24 flex-1`} aria-label="Watch group" value={group} onChange={(e) => setGroup(e.target.value)}>{[...new Set(["Watch", ...Object.keys(groups)])].map((name) => <option key={name}>{name}</option>)}</select>
      <button className={button} disabled={!selected} onClick={pin} title="Pin selected field or subtree to this group">Pin</button>
      {pinnedOnly && <button className={button} disabled={!selected || !(groups[group] ?? []).some((p) => p.path === selected.path)} onClick={() => setGroups((old) => ({...old, [group]: (old[group] ?? []).filter((p) => p.path !== selected?.path)}))}>Unpin</button>}
      {pinnedOnly && <><input className={`${field} min-w-24 flex-1`} aria-label="New watch group" placeholder="New group…" value={newGroup} onChange={(e) => setNewGroup(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter") addGroup(); }} /><button className={button} disabled={!newGroup.trim()} onClick={addGroup}>Add group</button></>}
    </div>
    {pinError && <p className="px-3 py-1 text-[11px] text-warn">{pinError}</p>}
    {error && <p role="alert" className="px-3 py-2 text-[12px] text-danger">{error}</p>}
    {visible.length > LIMIT && <p role="status" className="px-3 py-1 text-[12px] text-warn">Showing the first {LIMIT} fields. Collapse a group or filter to inspect others.</p>}
    <div className="min-h-0 flex-1">
      <SymbolTree roots={pinnedOnly ? pinnedRoots : roots} live={live} activeVariants={variants} filters={!pinnedOnly} emptyMessage={pinnedOnly ? "No available pins in this group. Browse all, select a field, then Pin." : undefined} selected={selected} onSelect={setSelected} onWatch={onWatch} watched={watched} notes={notes} onVisibleNodes={setVisible} label="Live Watch" />
    </div>
    <p className="border-t border-rule px-3 py-2 text-[11px] text-muted">Expand structs, arrays and pointers to read their fields. Press W to plot fixed-address numbers. Changed values briefly highlight. Reads do not halt the target.</p>
  </div>;
}
