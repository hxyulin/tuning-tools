import { useCallback, useRef, useState } from "react";
import { Popover, ghostButton } from "../ui";
import { host } from "../host";
import { OpenedElf } from "../elf/api";
import { CoreState, Stats } from "./api";
import { formatMicros, formatRate } from "./format";
import { useCapture } from "./useCapture";
import { StreamControl } from "./StreamControl";
import { Link } from "./useSession";

const coreText: Record<CoreState, string> = {
  running: "Running",
  halted: "Halted",
  sleeping: "Sleeping",
  lockedUp: "Locked up",
  unknown: "Unknown",
};

const linkText = {
  idle: "Not connected",
  connecting: "Connecting…",
  connected: "Connected",
  disconnected: "Disconnected",
  failed: "Connection failed",
};

function Cell({ children, title, tone }: { children: React.ReactNode; title?: string; tone?: "danger" | "warn" }) {
  return (
    <span
      title={title}
      className={`shrink-0 border-l border-rule px-3 py-[3px] tabular-nums ${tone === "danger" ? "text-danger" : tone === "warn" ? "text-warn" : ""}`}
    >
      {children}
    </span>
  );
}

export function StatusBar({ elf, link, stats }: { elf: OpenedElf | null; link: Link; stats: Stats | null }) {
  const { stream } = useCapture();
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => setOpen(false), []);
  const ramStatics = elf?.roots.filter((r) => !r.readOnly).length ?? 0;
  const slow = stats && stats.targetHz > 0 && stats.achievedHz < stats.targetHz * 0.9;
  const lit = link.state === "connected";

  return (
    <footer data-statusbar className="flex shrink-0 flex-wrap items-stretch border-t border-rule bg-surface text-[12px] whitespace-nowrap text-muted">
      {host.name !== "vscode" && <span className="ml-auto flex min-w-0 items-center gap-1.5 border-l border-rule px-3 py-[3px]">
        <span
          aria-hidden
          data-lit={lit || undefined}
          className={`h-[7px] w-[7px] shrink-0 rounded-full ${lit ? "bg-accent" : link.state === "failed" ? "bg-danger" : "bg-faint"}`}
        />
        <span className={link.state === "failed" ? "text-danger" : "text-ink"}>{linkText[link.state]}</span>
        {link.message && (
          <span className="min-w-0 truncate text-danger" title={link.message}>
            {link.message}
          </span>
        )}
      </span>}
      {lit && stats && (
        <>
          <Cell title="Achieved sample rate over the last 200 ticks" tone={slow ? "warn" : undefined}>
            {stats.targetHz > 0 ? `${formatRate(stats.achievedHz)} of ${stats.targetHz} Hz` : "Idle"}
          </Cell>
          {stats.skippedTicks > 0 && (
            <Cell title="Ticks skipped because reads ran past the next deadline" tone="warn">
              {stats.skippedTicks} skipped
            </Cell>
          )}
          {stats.failedRegions > 0 && (
            <Cell title={stats.lastError ?? undefined} tone="danger">
              {stats.failedRegions} failed reads
            </Cell>
          )}
          {link.carrier !== "serial" && stats.core !== "running" && <Cell tone="warn">Core {coreText[stats.core].toLowerCase()}</Cell>}
          {stats.log.state === "attached" && stats.log.blocking && <Cell tone="warn">RTT blocking</Cell>}
        </>
      )}
      <button ref={anchor} onClick={() => setOpen((v) => !v)} aria-expanded={open} aria-haspopup="dialog" className={`${ghostButton} ml-auto shrink-0 px-3`}>{stream.listening ? "Streaming · Diagnostics…" : stream.error ? "Stream error · Diagnostics…" : "Diagnostics…"}</button>
      <Popover anchor={anchor} open={open} onClose={close} place="above-right" label="Target diagnostics">
        <div className="grid max-w-[min(440px,calc(100vw-24px))] gap-2 p-2">
          <h2 className="font-semibold">Target diagnostics</h2>
      {elf && (
        <span className="min-w-0 truncate px-3 py-[3px]" title={elf.summary.path}>
          {elf.summary.machine}, {ramStatics} RAM statics, parsed in {elf.parseMs} ms
        </span>
      )}
          {lit && stats && <>
          {stats.values > 0 && (
            <Cell title={`Average read; worst ${formatMicros(stats.readMaxUs)}, jitter ${formatMicros(stats.jitterUs)}`}>
              {stats.values} values in {stats.regions} {stats.regions === 1 ? "read" : "reads"},{" "}
              {formatMicros(stats.readAvgUs)}
            </Cell>
          )}
          {link.carrier === "serial" ? (
            <Cell title="Framed link to the firmware over its USB port">USB link</Cell>
          ) : (
            <>
              <Cell tone={stats.core === "running" ? undefined : "warn"}>Core {coreText[stats.core].toLowerCase()}</Cell>
              {stats.log.state === "attached" && stats.log.blocking ? (
                <Cell
                  tone="warn"
                  title="This channel is in block-if-full mode (another tool may have set it). The firmware waits whenever the log buffer fills, so logging can stall control loops. Reset the board to restore the firmware's own mode."
                >
                  Log on RTT “{stats.log.channel}”, blocking
                </Cell>
              ) : (
                <Cell>
                  {stats.log.state === "attached"
                    ? `Log on RTT “${stats.log.channel}”`
                    : stats.log.state === "searching"
                      ? "Looking for RTT"
                      : "No RTT log"}
                </Cell>
              )}
            </>
          )}
          </>}
          {!elf && !lit && <p className="text-muted">Open firmware or connect a target to see diagnostics.</p>}
          <StreamControl />
        </div>
      </Popover>
    </footer>
  );
}
