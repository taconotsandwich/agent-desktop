import { mkdtemp, mkdir, readFile, rm, stat } from "node:fs/promises";
import { join, resolve } from "node:path";
import { tmpdir } from "node:os";
import assert from "node:assert/strict";

const root = resolve(import.meta.dir, "../..");
const packageDir = resolve(process.argv[2] ?? join(root, "target/npm"));
const manifest: { version: string } = JSON.parse(
  await readFile(join(packageDir, "launcher/package.json"), "utf8"),
);
const version = manifest.version;
const tarballs = [
  join(packageDir, `agent-desktop-linux-x64-${version}.tgz`),
  join(packageDir, `agent-desktop-${version}.tgz`),
];
const scratch = await mkdtemp(join(tmpdir(), "agent-desktop-package-"));
const home = join(scratch, "home with spaces");
const dataHome = join(scratch, "data with spaces");
const env: NodeJS.ProcessEnv = {
  ...process.env,
  HOME: home,
  XDG_DATA_HOME: dataHome,
  XDG_CONFIG_HOME: join(scratch, "config"),
  XDG_CACHE_HOME: join(scratch, "cache"),
  npm_config_cache: join(scratch, "npm-cache"),
};
delete env.DBUS_SESSION_BUS_ADDRESS;
delete env.XDG_RUNTIME_DIR;
await mkdir(home, { recursive: true });
async function run(command: string[], cwd = scratch, expected = 0) {
  const child = Bun.spawn(command, {
    cwd,
    env,
    stdout: "pipe",
    stderr: "pipe",
  });
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  assert.equal(code, expected, `${command.join(" ")}\n${stdout}\n${stderr}`);
  return { stdout, stderr };
}
try {
  const prefix = join(scratch, "global");
  await run([
    "npm",
    "install",
    "--global",
    "--prefix",
    prefix,
    "--offline",
    "--ignore-scripts",
    "--no-audit",
    "--no-fund",
    ...tarballs,
  ]);
  const installed = join(prefix, "bin/agent-desktop");
  assert.equal(
    (await run([installed, "--version"])).stdout.trim(),
    `agent-desktop ${version}`,
  );
  assert.match((await run([installed, "--help"])).stdout, /setup/);
  const invalid = await run(
    [installed, "--invalid-package-test-flag"],
    scratch,
    2,
  );
  assert.equal(invalid.stdout, "");
  await run([installed, "setup"]);
  const stable = join(
    dataHome,
    "agent-desktop/versions",
    version,
    "agent-desktop",
  );
  assert.ok((await stat(stable)).isFile());
  assert.equal(
    (await run([stable, "--version"])).stdout.trim(),
    `agent-desktop ${version}`,
  );
  await run([installed, "setup"]);
  const desktop = await readFile(
    join(dataHome, "applications/agent-desktop.desktop"),
    "utf8",
  );
  assert.ok(desktop.includes("agent-desktop/versions/"));
  const doctor = await run([installed, "doctor", "--json"], scratch, 1);
  JSON.parse(doctor.stdout);
  const npx = await run([
    "npx",
    "--offline",
    "--yes",
    "--package",
    tarballs[0],
    "--package",
    tarballs[1],
    "agent-desktop",
    "--version",
  ]);
  assert.equal(npx.stdout.trim(), `agent-desktop ${version}`);
  await run([
    "npx",
    "--offline",
    "--yes",
    "--package",
    tarballs[0],
    "--package",
    tarballs[1],
    "agent-desktop",
    "setup",
  ]);
  const runtime = join(scratch, "runtime");
  await mkdir(runtime, { mode: 0o700 });
  await run([
    "env",
    "XDG_RUNTIME_DIR=" + runtime,
    "dbus-run-session",
    "--",
    "bun",
    join(root, "packaging/npm/mcp-smoke.ts"),
    installed,
  ]);
  console.log(
    "PASS: global npm install, npx cache execution, version/help, exit status, setup with spaces, repeated setup, doctor JSON, MCP handshake and signal cleanup",
  );
} finally {
  await rm(scratch, { recursive: true, force: true });
}
