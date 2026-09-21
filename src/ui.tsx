import { CSSProperties, ReactNode, RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

export const button = "rounded-sm border border-rule bg-panel px-2.5 py-0.5 hover:bg-sunken disabled:opacity-50 disabled:hover:bg-panel";
export const primaryButton =
  "rounded-sm border border-accent bg-accent-wash px-2.5 py-0.5 font-medium hover:brightness-95 disabled:opacity-50";
export const ghostButton =
  "rounded-sm px-1.5 py-0.5 text-muted hover:bg-sunken hover:text-ink disabled:opacity-50 disabled:hover:bg-transparent";
export const field = "rounded-sm border border-rule bg-surface px-1.5 py-0.5 disabled:opacity-60";

interface Option<T> {
  value: T;
  label: ReactNode;
  title?: string;
  disabled?: boolean;
}

/** A row of mutually exclusive buttons; `value` null leaves every one released */
export function Segmented<T extends string | number>({
  label,
  options,
  value,
  onChange,
  disabled,
}: {
  label: string;
  options: Option<T>[];
  value: T | null;
  onChange: (value: T) => void;
  disabled?: boolean;
}) {
  return (
    <span role="radiogroup" aria-label={label} className="inline-flex rounded-sm border border-rule bg-panel p-px">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          disabled={disabled || o.disabled}
          title={o.title}
          onClick={() => onChange(o.value)}
          className={`rounded-[1px] px-2 py-px ${
            o.value === value
              ? "bg-surface text-ink shadow-[0_0_0_1px_var(--rule)]"
              : "text-muted enabled:hover:text-ink disabled:opacity-45"
          }`}
        >
          {o.label}
        </button>
      ))}
    </span>
  );
}

export function TabStrip({ children, tools }: { children: ReactNode; tools?: ReactNode }) {
  return (
    <div role="tablist" className="flex shrink-0 flex-wrap items-end gap-0.5 border-b border-rule bg-surface px-2 pt-1.5">
      {children}
      {tools && <div className="ml-auto flex min-w-0 items-center gap-1 pb-1 text-[12px]">{tools}</div>}
    </div>
  );
}

export function Tab({
  selected,
  onSelect,
  count,
  disabled,
  title,
  children,
}: {
  selected: boolean;
  onSelect: () => void;
  count?: number | null;
  disabled?: boolean;
  title?: string;
  children: ReactNode;
}) {
  return (
    <button
      role="tab"
      aria-selected={selected}
      disabled={disabled}
      title={title}
      onClick={onSelect}
      className={`-mb-px border-b-2 px-3 py-1.5 disabled:opacity-50 ${
        selected ? "border-accent text-ink" : "border-transparent text-muted enabled:hover:text-ink"
      }`}
    >
      {children}
      {count !== undefined && count !== null && <span className="ml-1 text-faint">{count}</span>}
    </button>
  );
}

/**
 * A panel under or over its anchor, closed by Escape or a click outside. Fixed to the viewport,
 * so a clipping parent (the status bar) does not cut it off.
 */
export function Popover({
  anchor,
  open,
  onClose,
  place,
  label,
  children,
}: {
  anchor: RefObject<HTMLElement | null>;
  open: boolean;
  onClose: () => void;
  /** Under the anchor's left edge, or over its right edge */
  place: "below-left" | "above-right";
  label: string;
  children: ReactNode;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const [at, setAt] = useState<CSSProperties | null>(null);

  useLayoutEffect(() => {
    if (!open || !anchor.current) return;
    const r = anchor.current.getBoundingClientRect();
    const width = panel.current?.offsetWidth ?? 300;
    const height = panel.current?.offsetHeight ?? 200;
    const left = place === "below-left" ? r.left : r.right - width;
    const top = place === "below-left" ? r.bottom + 4 : r.top - height - 4;
    setAt({
      left: Math.max(4, Math.min(left, window.innerWidth - width - 4)),
      top: Math.max(4, Math.min(top, window.innerHeight - height - 4)),
    });
  }, [open, anchor, place]);

  useEffect(() => {
    if (!open) return;
    const down = (e: PointerEvent) => {
      const t = e.target as Node;
      // A nested popover is portalled beside its parent, not inside it.
      const panels = [...document.querySelectorAll("[data-popover]")];
      const own = panels.indexOf(panel.current!);
      if (panels.slice(own + 1).some((p) => p.contains(t))) return;
      if (!panel.current?.contains(t) && !anchor.current?.contains(t)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape" && [...document.querySelectorAll("[data-popover]")].slice(-1)[0] === panel.current) {
        e.preventDefault();
        onClose();
        const target = anchor.current;
        (target?.matches("button") ? target : target?.querySelector("button"))?.focus();
      }
    };
    const focusFrame = requestAnimationFrame(() => {
      panel.current?.querySelector<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled), [tabindex='0']")?.focus();
    });
    const resize = () => onClose();
    document.addEventListener("pointerdown", down);
    document.addEventListener("keydown", key);
    window.addEventListener("resize", resize);
    return () => {
      cancelAnimationFrame(focusFrame);
      document.removeEventListener("pointerdown", down);
      document.removeEventListener("keydown", key);
      window.removeEventListener("resize", resize);
    };
  }, [open, onClose, anchor]);

  if (!open) return null;
  // Portalled so the panel inherits nothing from where its anchor sits
  // (the status bar's nowrap would stop its text wrapping)
  return createPortal(
    <div
      ref={panel}
      data-popover
      role="dialog"
      aria-label={label}
      style={at ?? { visibility: "hidden" }}
      className="fixed z-50 max-h-[calc(100vh-8px)] overflow-auto max-w-[calc(100vw-8px)] min-w-[220px] rounded-sm border border-rule bg-surface p-1 text-[12px] whitespace-normal text-ink shadow-lg"
    >
      {children}
    </div>,
    document.body,
  );
}

/** A row in a Popover used as a menu */
export function MenuItem({
  onSelect,
  disabled,
  title,
  children,
}: {
  onSelect: () => void;
  disabled?: boolean;
  title?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      title={title}
      onClick={onSelect}
      className="block w-full rounded-[1px] px-2 py-1 text-left enabled:hover:bg-sunken disabled:text-faint"
    >
      {children}
    </button>
  );
}
