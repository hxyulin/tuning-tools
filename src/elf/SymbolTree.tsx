import { useEffect, useMemo, useRef, useState } from "react";
import { host } from "../host";
import { Children, RootNode, SymbolNode } from "./api";

type ChildState = { status: "loading" } | { status: "error"; message: string } | ({ status: "ok" } & Children);

type Row =
  | { type: "namespace"; key: string; label: string; depth: number; count: number }
  | { type: "node"; key: string; node: SymbolNode; depth: number }
  | { type: "note"; key: string; text: string; depth: number; error?: boolean };

interface Namespace {
  key: string;
  label: string;
  namespaces: Map<string, Namespace>;
  symbols: RootNode[];
  count: number;
}

function buildNamespaces(roots: RootNode[]): Namespace {
  const top: Namespace = { key: "", label: "", namespaces: new Map(), symbols: [], count: 0 };
  for (const root of roots) {
    let ns = top;
    ns.count++;
    for (const segment of root.segments.slice(0, -1)) {
      let next = ns.namespaces.get(segment);
      if (!next) {
        next = { key: `${ns.key}::${segment}`, label: segment, namespaces: new Map(), symbols: [], count: 0 };
        ns.namespaces.set(segment, next);
      }
      next.count++;
      ns = next;
    }
    ns.symbols.push(root);
  }
  return top;
}

/** Numbers, and containers that may hold numbers, on a readable node. */
export function watchable(node: SymbolNode) {
  if (!node.readable || node.ref.steps.some((s) => s.kind === "deref")) return false;
  if (node.kind === "scalar" || node.kind === "enum") return node.scalar !== null && typeof node.scalar === "string";
  return node.kind === "struct" || node.kind === "array" || node.kind === "taggedEnum";
}

/** Live text shown in a row in place of its type */
export interface RowNote {
  text: string;
  title?: string;
  tone?: "ink" | "muted" | "accent" | "danger";
}

const noteTone: Record<NonNullable<RowNote["tone"]>, string> = {
  ink: "text-ink",
  muted: "text-muted",
  accent: "text-accent",
  danger: "text-danger",
};

interface Props {
  roots: RootNode[];
  selected: SymbolNode | null;
  onSelect: (node: SymbolNode) => void;
  /** Watch a node (a number, or the numbers inside it) */
  onWatch?: (node: SymbolNode) => void;
  watched?: Set<string>;
  /** Keyed by node path */
  notes?: Map<string, RowNote>;
  /** Show the RAM only / App only filters */
  filters?: boolean;
  label?: string;
  /** Numeric rows exposed by the current expansion/filter state. */
  onVisibleNodes?: (nodes: SymbolNode[]) => void;
}

export function SymbolTree({
  roots,
  selected,
  onSelect,
  onWatch,
  watched,
  notes,
  filters = true,
  label = "Symbols",
  onVisibleNodes,
}: Props) {
  const [filter, setFilter] = useState("");
  const [hideReadOnly, setHideReadOnly] = useState(true);
  const [hideInternal, setHideInternal] = useState(true);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [children, setChildren] = useState<Map<string, ChildState>>(new Map());
  const listRef = useRef<HTMLDivElement>(null);

  // New ELF: forget expansion and cached children
  useEffect(() => {
    setExpanded(new Set());
    setChildren(new Map());
  }, [roots]);

  const needle = filter.trim().toLowerCase();
  const visibleRoots = useMemo(
    () =>
      roots.filter(
        (r) =>
          (!filters || !hideReadOnly || !r.readOnly) &&
          (!filters || !hideInternal || !r.internal) &&
          (!needle || r.path.toLowerCase().includes(needle)),
      ),
    [roots, filters, hideReadOnly, hideInternal, needle],
  );
  const tree = useMemo(() => buildNamespaces(visibleRoots), [visibleRoots]);

  const rows = useMemo(() => {
    const out: Row[] = [];
    // While filtering, open every namespace so matches are visible
    const isOpen = (key: string) => (needle ? true : expanded.has(key));

    const pushNode = (node: SymbolNode, depth: number) => {
      out.push({ type: "node", key: node.path, node, depth });
      if (!node.expandable || !expanded.has(node.path)) return;
      const state = children.get(node.path);
      if (!state || state.status === "loading") {
        out.push({ type: "note", key: `${node.path}/loading`, text: "Loading…", depth: depth + 1 });
      } else if (state.status === "error") {
        out.push({ type: "note", key: `${node.path}/error`, text: state.message, depth: depth + 1, error: true });
      } else {
        for (const child of state.nodes) pushNode(child, depth + 1);
        if (state.total > state.nodes.length) {
          out.push({
            type: "note",
            key: `${node.path}/more`,
            text: `${state.total - state.nodes.length} more elements not shown`,
            depth: depth + 1,
          });
        }
      }
    };

    const walk = (ns: Namespace, depth: number) => {
      // Crates and modules first, `<T as Trait>` impl scopes after them
      const qualified = (label: string) => (label.startsWith("<") ? 1 : 0);
      const names = [...ns.namespaces.values()].sort(
        (a, b) => qualified(a.label) - qualified(b.label) || a.label.localeCompare(b.label),
      );
      for (const child of names) {
        out.push({ type: "namespace", key: child.key, label: child.label, depth, count: child.count });
        if (isOpen(child.key)) walk(child, depth + 1);
      }
      for (const sym of ns.symbols) pushNode(sym, depth);
    };
    walk(tree, 0);
    return out;
  }, [tree, expanded, children, needle]);

  useEffect(() => {
    onVisibleNodes?.(rows.flatMap((r) => r.type === "node" && r.node.readable && typeof r.node.scalar === "string" && (r.node.kind === "scalar" || r.node.kind === "enum" || r.node.kind === "pointer") ? [r.node] : []));
  }, [rows, onVisibleNodes]);

  const toggle = (key: string, node?: SymbolNode) => {
    const next = new Set(expanded);
    if (next.has(key)) {
      next.delete(key);
    } else {
      next.add(key);
      if (node && !children.has(key)) {
        setChildren((m) => new Map(m).set(key, { status: "loading" }));
        host
          .symbolChildren(node.ref)
          .then((c) => setChildren((m) => new Map(m).set(key, { status: "ok", ...c })))
          .catch((e) => setChildren((m) => new Map(m).set(key, { status: "error", message: String(e) })));
      }
    }
    setExpanded(next);
  };

  const selectedKey = selected?.path;
  const onKeyDown = (e: React.KeyboardEvent) => {
    const focusable = rows.filter((r) => r.type !== "note");
    const index = focusable.findIndex((r) => r.key === document.activeElement?.getAttribute("data-key"));
    const row = focusable[index];
    const focus = (i: number) => {
      const target = focusable[Math.max(0, Math.min(focusable.length - 1, i))];
      if (!target) return;
      listRef.current?.querySelector<HTMLElement>(`[data-key="${CSS.escape(target.key)}"]`)?.focus();
      if (target.type === "node") onSelect(target.node);
    };
    if ((e.key === "w" || e.key === "W") && row?.type === "node" && onWatch && watchable(row.node)) {
      onWatch(row.node);
    } else if (e.key === "ArrowDown") focus(index + 1);
    else if (e.key === "ArrowUp") focus(index - 1);
    else if (row && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
      const open = row.type === "namespace" ? needle !== "" || expanded.has(row.key) : expanded.has(row.key);
      const canOpen = row.type === "namespace" || (row.type === "node" && row.node.expandable);
      if (canOpen && (e.key === "ArrowRight") !== open) {
        toggle(row.key, row.type === "node" ? row.node : undefined);
      }
    } else return;
    e.preventDefault();
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 px-2 py-2">
        <input
          value={filter}
          onChange={(e) => setFilter(e.currentTarget.value)}
          placeholder="Filter by path"
          spellCheck={false}
          className="min-w-0 flex-1 rounded-sm border border-rule bg-plot px-2 py-1 font-mono text-[12px] placeholder:text-faint"
        />
        {filters && (
          <>
            <label className="flex shrink-0 items-center gap-1.5 text-muted">
              <input
                type="checkbox"
                checked={hideReadOnly}
                onChange={(e) => setHideReadOnly(e.currentTarget.checked)}
                className="accent-[var(--accent)]"
              />
              RAM only
            </label>
            <label
              title="Hide task pools, RTT buffers, and embassy and defmt state"
              className="flex shrink-0 items-center gap-1.5 text-muted"
            >
              <input
                type="checkbox"
                checked={hideInternal}
                onChange={(e) => setHideInternal(e.currentTarget.checked)}
                className="accent-[var(--accent)]"
              />
              App only
            </label>
          </>
        )}
      </div>

      <div
        ref={listRef}
        role="tree"
        aria-label={label}
        onKeyDown={onKeyDown}
        className="min-h-0 flex-1 overflow-auto py-1"
      >
        {rows.length === 0 && (
          <p className="px-4 py-6 text-muted">
            {needle
              ? `No symbol path contains “${filter.trim()}”.`
              : roots.length
                ? "Every static is hidden by the filters above."
                : "This ELF has no typed statics."}
          </p>
        )}
        {rows.map((row, i) => {
          const indent = { paddingLeft: 10 + row.depth * 14 };
          if (row.type === "note") {
            return (
              <div key={row.key} style={indent} className={`py-0.5 pl-5 text-[12px] ${row.error ? "text-danger" : "text-muted"}`}>
                <span className="pl-4">{row.text}</span>
              </div>
            );
          }
          const isNs = row.type === "namespace";
          const open = isNs ? needle !== "" || expanded.has(row.key) : expanded.has(row.key);
          const canOpen = isNs || row.node.expandable;
          const isSelected = !isNs && row.key === selectedKey;
          const tabbable = isSelected || (!selectedKey && i === 0);
          return (
            <div
              key={row.key}
              data-key={row.key}
              role="treeitem"
              aria-expanded={canOpen ? open : undefined}
              aria-selected={isSelected}
              tabIndex={tabbable ? 0 : -1}
              style={indent}
              onClick={() => {
                if (isNs) toggle(row.key);
                else onSelect(row.node);
              }}
              onDoubleClick={() => !isNs && row.node.expandable && toggle(row.key, row.node)}
              className={`group flex cursor-default items-center gap-1.5 border-l-2 pr-2 leading-[22px] select-none ${
                isSelected ? "border-accent bg-accent-wash" : "border-transparent hover:bg-sunken"
              }`}
            >
              <button
                tabIndex={-1}
                aria-hidden
                onClick={(e) => {
                  e.stopPropagation();
                  if (canOpen) toggle(row.key, isNs ? undefined : row.node);
                }}
                className={`w-3.5 shrink-0 text-center text-[10px] text-muted ${canOpen ? "" : "invisible"}`}
              >
                {open ? "▾" : "▸"}
              </button>
              {isNs ? (
                <>
                  <span title={row.label} className="min-w-0 truncate text-muted">{row.label}</span>
                  <span className="ml-auto pl-2 text-[11px] tabular-nums text-muted">{row.count}</span>
                </>
              ) : (
                <>
                  <span
                    title={row.node.path}
                    className={`max-w-[70%] shrink-0 truncate font-mono text-[12px] ${row.node.readable ? "" : "text-muted line-through"}`}
                  >
                    {row.node.label}
                  </span>
                  {notes?.has(row.key) ? (
                    <span
                      title={notes.get(row.key)!.title}
                      className={`ml-auto min-w-0 truncate pl-3 font-mono text-[11px] ${noteTone[notes.get(row.key)!.tone ?? "ink"]}`}
                    >
                      {notes.get(row.key)!.text}
                    </span>
                  ) : (
                    <span
                      title={row.node.wrapper ? `${row.node.typeName} inside ${row.node.wrapper}` : row.node.typeName}
                      className="ml-auto min-w-0 truncate pl-3 font-mono text-[11px] text-faint"
                    >
                      {row.node.typeName}
                    </span>
                  )}
                  {onWatch && watchable(row.node) && (
                    <button
                      tabIndex={-1}
                      onClick={(e) => {
                        e.stopPropagation();
                        onWatch(row.node);
                      }}
                      title={row.node.expandable ? "Watch the numbers inside (W)" : "Watch (W)"}
                      className={`shrink-0 rounded-sm px-1.5 text-[11px] leading-[18px] hover:bg-panel ${
                        watched?.has(row.node.path)
                          ? "text-accent"
                          : isSelected
                            ? "text-muted"
                            : "invisible text-muted group-hover:visible"
                      }`}
                    >
                      {watched?.has(row.node.path) ? "Watching" : "Watch"}
                    </button>
                  )}
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
