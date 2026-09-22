export interface Timing { seg1: number; seg2: number; sjw: number; prescaler: number }
export interface ChannelConfig {
  channel: number; fd: boolean; arbitrationBitrate: number; dataBitrate: number;
  arbitrationSamplePoint: number; dataSamplePoint: number;
  arbitrationTiming: Timing | null; dataTiming: Timing | null;
}
export interface CanFrame {
  sequence: number; deviceTimestamp: string; hostTimestampNs: string;
  channel: number; id: number; extended: boolean; fd: boolean; brs: boolean;
  rtr: boolean; esi: boolean; ack: boolean; dlc: number; direction: "rx" | "tx" | "error"; data: number[];
}
export interface CanSnapshot {
  connected: boolean; devices: { index: number; model: string; channels: number; version: string }[];
  configs: ChannelConfig[]; frames: CanFrame[]; sequence: number; received: number;
  transmitted: number; errors: number; dropped: number; skipped: number;
  recording: string | null; recorded: number; error: string | null;
}
export type CanRequest =
  | { action: "scan"; library: string | null }
  | { action: "connect"; index: number; channels: number; configs: ChannelConfig[] }
  | { action: "disconnect" | "stopRecording" }
  | { action: "poll"; after: number }
  | { action: "record"; path: string }
  | { action: "send"; frame: { channel: number; id: number; extended: boolean; fd: boolean; brs: boolean; rtr: boolean; data: number[] } };
