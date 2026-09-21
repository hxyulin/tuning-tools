import { useEffect, useId, useState } from "react";
import { ConnectDefaults, host } from "../host";
import { Segmented, button, field, primaryButton } from "../ui";
import type * as api from "./api";
import { useHostStatus } from "./useHostStatus";
import { Link } from "./useSession";

const RATES = [10, 50, 100, 200, 500, 1000];
/** SWD clock; faster makes large reads quicker but needs short, clean wiring */
const SPEEDS = [1000, 4000, 10000];
const SETTINGS_KEY = "connection";

interface Settings {
  carrier: api.Carrier;
  port: string;
  probe: string;
  chip: string;
  rateHz: number;
  speedKhz: number;
}

function loadSettings(): Settings {
  const fallback: Settings = { carrier: "probe", port: "", probe: "", chip: "", rateHz: 100, speedKhz: 4000 };
  try {
    return { ...fallback, ...JSON.parse(host.storage.get(SETTINGS_KEY) ?? "{}") };
  } catch {
    return fallback;
  }
}

interface Props {
  link: Link;
  /** An ELF is open, which the probe needs */
  canConnect: boolean;
  onConnect: (request: api.ConnectRequest) => void;
  onDisconnect: () => void;
  /** Settings the host's launch configuration chose; shown without being remembered */
  preset: api.ConnectRequest | null;
  /** Settings the host suggests, used where none are remembered */
  defaults: ConnectDefaults | null;
}

export function ConnectBar({ link, canConnect, onConnect, onDisconnect, preset, defaults }: Props) {
  const [settings, setSettings] = useState(loadSettings);
  const { sources, notice } = useHostStatus();
  const [dismissed, setDismissed] = useState<number | null>(null);
  const [probes, setProbes] = useState<api.ProbeInfo[] | null>(null);
  const [chips, setChips] = useState<string[]>([]);
  const [ports, setPorts] = useState<api.PortInfo[] | null>(null);
  const chipList = useId();
  const [settingsOpen, setSettingsOpen] = useState(false);
  const active = link.state === "connecting" || link.state === "connected";

  const update = (patch: Partial<Settings>) => {
    const next = { ...settings, ...patch };
    setSettings(next);
    try {
      host.storage.set(SETTINGS_KEY, JSON.stringify(next));
    } catch {
      // Not remembered next launch; nothing else depends on it
    }
  };

  useEffect(() => {
    if (!preset) return;
    setSettings((s) => ({
      carrier: preset.carrier,
      probe: preset.probe ?? "",
      chip: preset.chip,
      rateHz: preset.rateHz,
      speedKhz: preset.speedKhz ?? s.speedKhz,
      port: preset.port ?? s.port,
    }));
  }, [preset]);

  useEffect(() => {
    if (!defaults) return;
    const remembered = host.storage.get(SETTINGS_KEY) !== null;
    setSettings((s) => ({
      ...s,
      chip: s.chip || defaults.chip || "",
      probe: s.probe || defaults.probe || "",
      speedKhz: remembered ? s.speedKhz : (defaults.speedKhz ?? s.speedKhz),
    }));
  }, [defaults]);

  const refreshProbes = () => {
    setProbes(null);
    host.listProbes().then(setProbes, () => setProbes([]));
  };
  useEffect(refreshProbes, []);

  const refreshPorts = () => {
    setPorts(null);
    host.listSerialPorts().then(setPorts, () => setPorts([]));
  };
  useEffect(refreshPorts, []);

  useEffect(() => {
    const query = settings.chip.trim();
    if (query.length < 3) return setChips([]);
    let stale = false;
    host.searchChips(query).then((names) => !stale && setChips(names));
    return () => {
      stale = true;
    };
  }, [settings.chip]);

  const connect = () =>
    onConnect({
        carrier: settings.carrier,
        probe: settings.probe || null,
        chip: settings.chip.trim(),
        speedKhz: settings.speedKhz,
        port: port || null,
      rateHz: settings.rateHz,
    });

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    if (active) onDisconnect();
    else if (!blocked) { setSettingsOpen(false); connect(); }
  };

  const serial = settings.carrier === "serial";
  const selectedMissing = settings.probe && probes && !probes.some((p) => p.selector === settings.probe);
  // Fall back to the first firmware port when the remembered one is gone
  const portMissing = settings.port && ports && !ports.some((p) => p.path === settings.port);
  const port = settings.port || ports?.find((p) => p.telemetry)?.path || "";
  const source = sources[settings.carrier];
  const blocked = !source.available
    ? (source.reason ?? "Not available here")
    : serial
      ? !port
        ? "Plug in the robot's USB cable, then rescan"
        : null
      : !canConnect
        ? "Open the firmware ELF first"
        : !settings.chip.trim()
          ? "Enter the target chip"
          : null;
  const shownNotice = notice && notice.id !== dismissed ? notice : null;

  return (
    <form onSubmit={submit} className="flex w-full flex-wrap items-center gap-2">
      <span className={`h-2 w-2 rounded-full ${link.state === "connected" ? "bg-good" : "bg-faint"}`} aria-hidden />
      <span className="font-medium">{serial ? "USB target" : settings.chip || "Debug probe"}</span>
      <span className="text-muted">{link.state === "connected" ? "Connected" : link.state === "connecting" ? "Connecting…" : link.state === "failed" ? "Connection failed" : "Disconnected"}</span>
      <span className="flex-1" />
      <button type="button" className={button} aria-expanded={settingsOpen} onClick={() => setSettingsOpen((v) => !v)}>Connection settings…</button>
      <button type="submit" disabled={!active && blocked !== null} title={active ? undefined : (blocked ?? undefined)} className={active ? button : primaryButton}>
        {link.state === "connecting" ? "Cancel" : active ? "Disconnect" : "Connect"}
      </button>
      {shownNotice && (
        <span role="status" className="flex items-center gap-1.5 rounded-sm border border-rule bg-sunken px-2 py-px">
          <span className="text-muted">{shownNotice.message}</span>
          {shownNotice.reconnect && !active && (
            <button
              type="button"
              disabled={blocked !== null}
              title={blocked ?? undefined}
              onClick={() => {
                setDismissed(shownNotice.id);
                connect();
              }}
              className={button}
            >
              Reconnect
            </button>
          )}
          <button type="button" onClick={() => setDismissed(shownNotice.id)} title="Dismiss" aria-label="Dismiss" className="text-muted hover:text-ink">
            ×
          </button>
        </span>
      )}
      {settingsOpen && <div className="connection-settings w-full">
        <div className="mt-3 flex flex-wrap items-center gap-3 border-t border-rule pt-3">
      <Segmented<api.Carrier | "debugger">
        label="Connect through"
        value={settings.carrier}
        disabled={active}
        onChange={(carrier) => carrier !== "debugger" && update({ carrier })}
        options={[
          {
            value: "probe",
            label: "Probe",
            // Still selectable when blocked, so the Connect button can say why
            title: sources.probe.reason ?? "Take the debug probe for this app alone; needs the ELF",
          },
          {
            value: "serial",
            label: "USB",
            disabled: !sources.serial.available,
            title: sources.serial.reason ?? "The robot's USB cable; no probe or ELF needed",
          },

        ]}
      />
      {serial ? (
        <>
          <label className="flex items-center gap-1.5">
            <span className="text-muted">Port</span>
            <select value={port} onChange={(e) => update({ port: e.currentTarget.value })} disabled={active} className={`${field} max-w-60`}>
              {!port && <option value="">{ports === null ? "Looking for ports…" : "No robot found"}</option>}
              {ports?.map((p) => (
                <option key={p.path} value={p.path}>
                  {p.path.replace(/^\/dev\//, "")}
                  {p.product ? ` (${p.product})` : ""}
                </option>
              ))}
              {portMissing && <option value={settings.port}>{settings.port} (not attached)</option>}
            </select>
          </label>
          <button type="button" onClick={refreshPorts} disabled={active} title="Look for serial ports again" className={button}>
            Rescan
          </button>
        </>
      ) : (
        <>
          <label className="flex items-center gap-1.5">
            <span className="text-muted">Probe</span>
            <select
              value={settings.probe}
              onChange={(e) => update({ probe: e.currentTarget.value })}
              disabled={active}
              className={`${field} max-w-40`}
            >
              <option value="">{probes === null ? "Looking for probes…" : probes.length ? "First one found" : "None found"}</option>
              {probes?.map((p) => (
                <option key={p.selector} value={p.selector}>
                  {p.name}
                  {p.serial ? ` (${p.serial})` : ""}
                </option>
              ))}
              {selectedMissing && <option value={settings.probe}>{settings.probe} (not attached)</option>}
            </select>
          </label>
          <button type="button" onClick={refreshProbes} disabled={active} title="Look for probes again" className={button}>
            Rescan
          </button>
          <label className="flex items-center gap-1.5">
            <span className="text-muted">Chip</span>
            <input
              value={settings.chip}
              onChange={(e) => update({ chip: e.currentTarget.value })}
              disabled={active}
              list={chipList}
              placeholder="STM32H723VG"
              spellCheck={false}
              className={`${field} w-32 font-mono text-[12px] placeholder:text-faint`}
            />
            <datalist id={chipList}>
              {chips.map((c) => (
                <option key={c} value={c} />
              ))}
            </datalist>
          </label>
        </>
      )}
      <details className="w-full">
        <summary className="cursor-pointer text-muted">Advanced · {settings.rateHz} Hz{!serial && ` · ${settings.speedKhz / 1000} MHz SWD`}</summary>
        <div className="mt-2 flex flex-wrap items-center gap-3">
          {!serial && (<>
          <label className="flex items-center gap-1.5" title="SWD clock speed. Lower it if reads fail on long or noisy wiring.">
            <span className="text-muted">SWD</span>
            <select
              value={settings.speedKhz}
              onChange={(e) => update({ speedKhz: Number(e.currentTarget.value) })}
              disabled={active}
              className={field}
            >
              {SPEEDS.map((k) => (
                <option key={k} value={k}>
                  {k / 1000} MHz
                </option>
              ))}
            </select>
          </label>
          </>)}
      <label className="flex items-center gap-1.5">
        <span className="text-muted">Rate</span>
        <select
          value={settings.rateHz}
          onChange={(e) => {
            const rateHz = Number(e.currentTarget.value);
            update({ rateHz });
            if (link.state === "connected") void host.setRate(rateHz);
          }}
          className={field}
        >
          {RATES.map((r) => (
            <option key={r} value={r}>
              {r} Hz
            </option>
          ))}
        </select>
      </label>
        </div>
      </details>
        </div>
      </div>}
      {!active && blocked && <p className="w-full text-[12px] text-muted">{blocked}. {!settingsOpen && "Use Connection settings to choose your target."}</p>}
    </form>
  );
}
