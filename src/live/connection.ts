import type { Carrier, PortInfo, ProbeInfo } from "./api";

/** A remembered device must never silently resolve to a different robot. */
export function connectionProblem({ carrier, port, probe, chip, ports, probes, canConnect, source, scanError }: {
  carrier: Carrier; port: string; probe: string; chip: string;
  ports: PortInfo[] | null; probes: ProbeInfo[] | null; canConnect: boolean;
  source: { available: boolean; reason: string | null }; scanError: string | null;
}): string | null {
  if (!source.available) return source.reason ?? "Not available here";
  if (scanError) return scanError;
  if (carrier === "serial") {
    if (ports === null) return "Looking for USB ports…";
    if (port && !ports.some((p) => p.path === port)) return "The selected USB port is missing. Reattach it or choose another port";
    if (!port) return "Plug in the robot’s USB cable, then select a port";
    return null;
  }
  if (probes === null) return "Looking for debug probes…";
  if (probe && !probes.some((p) => p.selector === probe)) return "The selected probe is missing. Reattach it or choose another probe";
  if (!probes.length) return "No debug probe found. Attach your probe";
  if (!canConnect) return "Open the firmware ELF first";
  if (!chip.trim()) return "Enter the target chip";
  return null;
}
