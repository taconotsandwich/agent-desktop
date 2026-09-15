import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { createInterface } from "node:readline";
import assert from "node:assert/strict";

const binary = process.argv[2];
if (!binary) throw new Error("Provide installed npm launcher");
async function exercise(signal: boolean) {
  const child = spawn(binary, ["--session", "x11", "--desktop", "kde"], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  const lines = createInterface({ input: child.stdout });
  const iterator = lines[Symbol.asyncIterator]();
  let errors = "";
  child.stderr.on("data", (chunk) => {
    errors += String(chunk);
  });
  const exit = new Promise<{
    code: number | null;
    signal: NodeJS.Signals | null;
  }>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  async function cleanup() {
    try {
      const native = (
        await readFile(`/proc/${child.pid}/task/${child.pid}/children`, "utf8")
      )
        .trim()
        .split(/\s+/)
        .filter(Boolean);
      for (const pid of native) {
        try {
          process.kill(Number(pid), "SIGKILL");
        } catch {}
      }
    } catch {}
    child.kill("SIGKILL");
  }
  const timer = setTimeout(() => {
    void cleanup();
  }, 30000);
  try {
    child.stdin.write(
      JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: {
          protocolVersion: "2025-03-26",
          capabilities: {},
          clientInfo: { name: "npm-smoke", version: "1" },
        },
      }) + "\n",
    );
    let initialized = false;
    while (!initialized) {
      const line = await iterator.next();
      assert.equal(line.done, false, errors);
      const message: {
        id?: number;
        result?: { serverInfo?: { name: string } };
        error?: unknown;
      } = JSON.parse(line.value!);
      if (message.id === 1) {
        assert.equal(message.error, undefined);
        assert.ok(message.result?.serverInfo);
        initialized = true;
      }
    }
    child.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) +
        "\n",
    );
    const children = (
      await readFile(`/proc/${child.pid}/task/${child.pid}/children`, "utf8")
    )
      .trim()
      .split(/\s+/)
      .filter(Boolean);
    assert.equal(children.length, 1, "launcher owns one native server");
    if (signal) child.kill("SIGTERM");
    else child.stdin.end();
    const result = await exit;
    if (signal) assert.equal(result.signal, "SIGTERM");
    else assert.equal(result.code, 0, errors);
    for (const pid of children) {
      assert.throws(() => process.kill(Number(pid), 0), { code: "ESRCH" });
    }
  } finally {
    clearTimeout(timer);
    lines.close();
    await cleanup();
    await exit;
  }
}
await exercise(false);
await exercise(true);
console.log(
  "PASS: MCP initialize on clean stdout, EOF shutdown, SIGTERM forwarding, native process cleanup",
);
