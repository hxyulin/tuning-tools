import { useCallback, useRef, useState } from "react";
import { Catalog } from "../elf/api";
import { host } from "../host";
import type * as api from "./api";
import { saveUnsupported } from "./tuningPresentation";
import { samples } from "./samples";

const MAX_LOG_LINES = 5000;

export interface Link {
  state: api.LinkState | "idle";
  message: string | null;
  carrier: api.Carrier | null;
}

export interface Tune {
  check: api.CatalogCheck;
  /** Unknown until a SAVE reply explicitly reports support or lack of it. */
  saveSupported?: boolean;
  values: Map<number, api.TuneValue>;
  /**
   * Requested values as of the first read or the last save in this session. The firmware does
   * not report what its flash holds, so this is the best guess at what the robot boots with.
   */
  saved: Map<number, number>;
}

function requestedValues(values: Map<number, api.TuneValue>) {
  const out = new Map<number, number>();
  for (const [id, v] of values) if (v.requested !== null) out.set(id, v.requested);
  return out;
}

export function useSession() {
  const [link, setLink] = useState<Link>({ state: "idle", message: null, carrier: null });
  /** Sent by the firmware over a framed link */
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [stats, setStats] = useState<api.Stats | null>(null);
  const [logs, setLogs] = useState<api.LogLine[]>([]);
  const [tune, setTune] = useState<Tune | null>(null);
  // Events from a replaced connection are ignored
  const generation = useRef(0);

  const connect = useCallback(async (request: api.ConnectRequest) => {
    const gen = ++generation.current;
    const live = () => gen === generation.current;
    samples.clear();
    setStats(null);
    setLogs([]);
    setTune(null);
    setCatalog(null);
    const carrier = request.carrier;
    setLink({ state: "connecting", message: null, carrier });

    try {
      await host.connect(request, {
        frame: (frame) => {
          if (live()) samples.ingest(frame);
        },
        event: (event) => {
          if (!live()) return;
          if (event.type === "status") {
            setLink({ state: event.state, message: event.message, carrier });
            if (event.state !== "connected") {
              setStats(null);
              setTune(null);
            }
          } else if (event.type === "catalog") {
            setCatalog(event.catalog);
          } else if (event.type === "tune") {
            const values = new Map(event.values.map((v) => [v.id, v]));
            setTune((old) => ({
              check: event.check,
              saveSupported: old?.saveSupported,
              values,
              saved: event.savedValues ? new Map(event.savedValues) : old?.saved.size ? old.saved : requestedValues(values),
            }));
          } else if (event.type === "stats") {
            setStats(event);
          } else {
            setLogs((old) => {
              const next = old.concat(event.lines);
              return next.length > MAX_LOG_LINES ? next.slice(next.length - MAX_LOG_LINES) : next;
            });
          }
        },
      });
    } catch (e) {
      if (live()) setLink({ state: "failed", message: String(e), carrier });
    }
  }, []);

  const disconnect = useCallback(async () => {
    try {
      await host.disconnect();
      // Ignore late events from a cancelled or disconnected attempt.
      ++generation.current;
      setTune(null);
      setStats(null);
      setLink((l) => ({ state: "disconnected", message: null, carrier: l.carrier }));
    } catch (e) {
      setLink((l) => ({ ...l, message: `Could not disconnect: ${String(e)}` }));
    }
  }, []);

  /** Keep every requested tuning value across a power cycle */
  const save = useCallback(async () => {
    const gen = generation.current;
    try {
      await host.saveValues();
      if (gen === generation.current) setTune((t) => t && { ...t, saveSupported: true, saved: requestedValues(t.values) });
    } catch (error) {
      if (gen === generation.current && saveUnsupported(error)) setTune((t) => t && { ...t, saveSupported: false });
      throw error;
    }
  }, []);

  const clearLogs = useCallback(() => setLogs([]), []);

  return { link, stats, logs, tune, catalog, connect, disconnect, save, clearLogs };
}
