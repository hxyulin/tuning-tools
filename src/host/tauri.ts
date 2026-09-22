import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import * as elf from "../elf/api";
import * as live from "../live/api";
import * as rec from "../live/recording";
import { STANDALONE_STATUS, fixedStatus, localStorageBacked } from "./common";
import type { Host } from "./types";

/** Typed-array views need the frame to start at an 8-byte boundary */
function toAlignedBuffer(message: ArrayBuffer | Uint8Array | number[]): ArrayBuffer {
  if (message instanceof ArrayBuffer) return message;
  if (message instanceof Uint8Array) {
    return message.buffer.slice(message.byteOffset, message.byteOffset + message.byteLength) as ArrayBuffer;
  }
  return new Uint8Array(message).buffer;
}

/** The desktop app: Tauri commands, and channels for the session's streams. */
export const tauriHost: Host = {
  name: "tauri",
  canRequest: (request) => invoke("can_request", { request }),
  storage: localStorageBacked,
  async startup() {
    return { elfPath: await elf.startupElfPath(), connect: null, connectDefaults: null, watches: [] };
  },
  watchStatus: fixedStatus(STANDALONE_STATUS),
  async pickElf() {
    const path = await open({ title: "Open firmware ELF", multiple: false, directory: false });
    return typeof path === "string" ? path : null;
  },
  openElf: elf.openElf,
  symbolChildren: elf.symbolChildren,
  inspectChildren: elf.inspectChildren,
  nodeMetadata: elf.nodeMetadata,
  watchableLeaves: live.watchableLeaves,
  listProbes: live.listProbes,
  listSerialPorts: live.listSerialPorts,
  searchChips: live.searchChips,
  connect(request, handlers) {
    const data = new Channel<ArrayBuffer | Uint8Array | number[]>((frame) => handlers.frame(toAlignedBuffer(frame)));
    const events = new Channel<live.SessionEvent>(handlers.event);
    return live.connect(request, data as Channel<ArrayBuffer>, events);
  },
  disconnect: live.disconnect,
  setRate: live.setRate,
  setWatches: live.setWatches,
  requestValue: live.requestValue,
  saveValues: live.saveValues,
  discardValues: live.discardValues,
  taskStates: live.taskStates,
  taskTrace: live.taskTrace,
  readValues: live.readValues,
  recordingStart: rec.recordingStart,
  recordingStop: rec.recordingStop,
  exportCsv: rec.exportCsv,
  streamStart: rec.streamStart,
  streamStop: rec.streamStop,
  appState: rec.appState,
  watchAppEvents(listener) {
    let stop: (() => void) | null = null;
    let gone = false;
    listen<rec.AppEvent>(rec.APP_EVENT, (e) => listener(e.payload)).then((unlisten) => {
      if (gone) unlisten();
      else stop = unlisten;
    });
    return () => {
      gone = true;
      stop?.();
    };
  },
  async pickRecordingPath() {
    const path = await save({ title: "Record to", filters: [{ name: "MCAP recording", extensions: ["mcap"] }] });
    return path ?? null;
  },
  async pickCsvPath(suggested) {
    const path = await save({
      title: "Export CSV",
      defaultPath: suggested,
      filters: [{ name: "CSV", extensions: ["csv"] }],
    });
    return path ?? null;
  },
  async pickRecording() {
    const path = await open({
      title: "Export a recording as CSV",
      multiple: false,
      directory: false,
      filters: [{ name: "MCAP recording", extensions: ["mcap"] }],
    });
    return typeof path === "string" ? path : null;
  },
  reveal: (path) => revealItemInDir(path),
};
