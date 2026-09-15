#!/usr/bin/env node
import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, isAbsolute } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const metadata = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const args = process.argv.slice(2);
if (process.platform !== "linux" || process.arch !== "x64") {
  console.error("agent-desktop supports Linux x64 with glibc. Run it on your Linux desktop host.");
  process.exit(1);
}
let packaged;
try {
  packaged = join(dirname(require.resolve("@taconotsandwich/agent-desktop-linux-x64/package.json")), "bin/agent-desktop");
} catch {
  console.error("The Linux binary package is missing. Reinstall with optional dependencies enabled.");
  process.exit(1);
}
const dataHome = process.env.XDG_DATA_HOME || join(homedir(), ".local/share");
if (!isAbsolute(dataHome)) {
  console.error("XDG_DATA_HOME must be an absolute path.");
  process.exit(1);
}
const installed = join(dataHome, "agent-desktop/versions", metadata.version, "agent-desktop");
const management = args.includes("setup") || args.includes("doctor");
const binary = !management && existsSync(installed) ? installed : packaged;
const child = spawn(binary, args, { stdio: "inherit" });
const signals = ["SIGINT", "SIGTERM", "SIGHUP"];
const forward = new Map(signals.map(signal => [signal, () => child.kill(signal)]));
for (const [signal, handler] of forward) process.on(signal, handler);
child.once("error", error => {
  console.error(`Cannot start agent-desktop: ${error.message}`);
  process.exitCode = 1;
});
child.once("exit", (code, signal) => {
  for (const [name, handler] of forward) process.off(name, handler);
  if (signal) process.kill(process.pid, signal);
  else process.exitCode = code ?? 1;
});
