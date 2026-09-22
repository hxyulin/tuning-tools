import { cpSync, rmSync } from "node:fs";
rmSync("src-tauri/frontend", { recursive: true, force: true });
cpSync("dist", "src-tauri/frontend", { recursive: true });
