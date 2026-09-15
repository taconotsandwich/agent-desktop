import {
  chmod,
  copyFile,
  mkdir,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { resolve, join } from "node:path";

const root = resolve(import.meta.dir, "../..");
const args = process.argv.slice(2);
if (args.length && (args.length !== 2 || args[0] !== "--binary")) {
  throw new Error(
    "Usage: bun packaging/npm/build.ts [--binary /path/to/agent-desktop]",
  );
}
if (process.platform !== "linux" || process.arch !== "x64") {
  throw new Error("Build Linux x64 packages on a Linux x64 glibc host.");
}
const manifest: unknown = Bun.TOML.parse(
  await Bun.file(join(root, "Cargo.toml")).text(),
);
if (
  typeof manifest !== "object" ||
  manifest === null ||
  !("package" in manifest)
)
  throw new Error("Missing Cargo package");
const pkg: unknown = manifest.package;
if (
  typeof pkg !== "object" ||
  pkg === null ||
  !("version" in pkg) ||
  typeof pkg.version !== "string"
)
  throw new Error("Missing Cargo version");
const version = pkg.version;
async function run(command: string[], cwd = root) {
  const child = Bun.spawn(command, {
    cwd,
    stdout: "inherit",
    stderr: "inherit",
  });
  if (await child.exited) throw new Error(`Failed: ${command.join(" ")}`);
}
if (!args.length) await run(["cargo", "build", "--release", "--locked"]);
const binary = resolve(args[1] ?? join(root, "target/release/agent-desktop"));
const check = Bun.spawn([binary, "--version"], {
  stdout: "pipe",
  stderr: "inherit",
});
const actual = (await new Response(check.stdout).text()).trim();
if ((await check.exited) !== 0 || actual !== `agent-desktop ${version}`)
  throw new Error("Binary version does not match Cargo.toml");
const out = join(root, "target/npm");
await mkdir(out, { recursive: true });
for (const name of await readdir(out)) {
  if (name.startsWith("agent-desktop-") && name.endsWith(".tgz"))
    await rm(join(out, name));
}
const launcher = join(out, "launcher");
const platform = join(out, "linux-x64");
for (const dir of [launcher, platform]) {
  await rm(dir, { recursive: true, force: true });
  await mkdir(join(dir, "bin"), { recursive: true });
  await copyFile(join(root, "LICENSE"), join(dir, "LICENSE"));
}
const common = {
  version,
  license: "MIT",
  repository: {
    type: "git",
    url: "git+https://github.com/taconotsandwich/agent-desktop.git",
  },
};
await writeFile(
  join(launcher, "package.json"),
  JSON.stringify(
    {
      ...common,
      name: "@taconotsandwich/agent-desktop",
      description: "Linux desktop control MCP server for KDE and GNOME",
      type: "module",
      engines: { node: ">=20" },
      bin: { "agent-desktop": "bin/agent-desktop.mjs" },
      files: ["bin", "LICENSE"],
      optionalDependencies: {
        "@taconotsandwich/agent-desktop-linux-x64": version,
      },
    },
    null,
    2,
  ) + "\n",
);
await writeFile(
  join(platform, "package.json"),
  JSON.stringify(
    {
      ...common,
      name: "@taconotsandwich/agent-desktop-linux-x64",
      description: "Linux x64 glibc binary for agent-desktop",
      os: ["linux"],
      cpu: ["x64"],
      libc: ["glibc"],
      files: ["bin", "LICENSE"],
    },
    null,
    2,
  ) + "\n",
);
await copyFile(
  join(root, "packaging/npm/launcher.mjs"),
  join(launcher, "bin/agent-desktop.mjs"),
);
await copyFile(binary, join(platform, "bin/agent-desktop"));
await chmod(join(launcher, "bin/agent-desktop.mjs"), 0o755);
await chmod(join(platform, "bin/agent-desktop"), 0o755);
await run(
  [
    "bun",
    "pm",
    "pack",
    "--ignore-scripts",
    "--filename",
    join(out, `agent-desktop-linux-x64-${version}.tgz`),
  ],
  platform,
);
await run(
  [
    "bun",
    "pm",
    "pack",
    "--ignore-scripts",
    "--filename",
    join(out, `agent-desktop-${version}.tgz`),
  ],
  launcher,
);
console.log(`Packages ready in ${out}; built for this host's glibc baseline.`);
