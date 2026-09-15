import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "../..");
const scratch = await mkdtemp(join(tmpdir(), "agent-desktop-skill-"));
const cli = "skills@1.5.26";
async function run(args: string[]) {
  const child = Bun.spawn(["bunx", cli, ...args], {
    cwd: scratch,
    env: { ...process.env, DO_NOT_TRACK: "1", DISABLE_TELEMETRY: "1" },
    stdout: "pipe",
    stderr: "pipe",
  });
  const timer = setTimeout(() => child.kill(), 60_000);
  try {
    const [stdout, stderr, code] = await Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]);
    assert.equal(code, 0, `${args.join(" ")}\n${stdout}\n${stderr}`);
    return stdout;
  } finally {
    clearTimeout(timer);
  }
}

try {
  await run(["add", root, "--list"]);
  const result: unknown = JSON.parse(
    await run([
      "add",
      root,
      "--skill",
      "agent-desktop",
      "--agent",
      "codex",
      "--yes",
      "--json",
    ]),
  );
  assert.ok(Array.isArray(result));
  assert.equal(result.length, 1);
  const installation: unknown = result[0];
  assert.ok(typeof installation === "object" && installation !== null);
  assert.ok("status" in installation && installation.status === "installed");
  assert.ok("scope" in installation && installation.scope === "project");
  const installed = join(scratch, ".agents/skills/agent-desktop");
  assert.ok("path" in installation && installation.path === installed);
  assert.deepEqual(
    await readFile(join(installed, "SKILL.md")),
    await readFile(join(root, "skills/agent-desktop/SKILL.md")),
  );
  const listed: unknown = JSON.parse(
    await run(["list", "--agent", "codex", "--json"]),
  );
  assert.ok(Array.isArray(listed));
  assert.ok(
    listed.some(
      (item: unknown) =>
        typeof item === "object" &&
        item !== null &&
        "name" in item &&
        item.name === "agent-desktop" &&
        "scope" in item &&
        item.scope === "project" &&
        "path" in item &&
        item.path === installed,
    ),
  );
  console.log(
    `PASS: ${cli} repository discovery, isolated Codex installation, listing and installed content`,
  );
} finally {
  await rm(scratch, { recursive: true, force: true });
}
