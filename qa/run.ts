import { cp, mkdir, mkdtemp, rm, open, writeFile } from "node:fs/promises";
import { resolve, join, basename } from "node:path";
import { tmpdir } from "node:os";

const [suite, desktop, protocol, ...extra] = process.argv.slice(2);
if (
  extra.length ||
  !["desktop", "farm"].includes(suite ?? "") ||
  !["KDE", "GNOME"].includes(desktop ?? "") ||
  !["wayland", "x11"].includes(protocol ?? "") ||
  (desktop === "GNOME" && protocol !== "wayland") ||
  (suite === "farm" && (desktop !== "KDE" || protocol !== "wayland"))
) {
  throw new Error(
    "Usage: bun qa/run.ts <desktop|farm> <KDE|GNOME> <wayland|x11>; GNOME requires Wayland, farm requires KDE/Wayland",
  );
}

const root = resolve(import.meta.dir, "..");
const row = `${suite}-${desktop.toLowerCase()}-${protocol}`;
const image = process.env.QA_IMAGE ?? "localhost/agent-desktop-qa:fedora43";
const artifacts = resolve(
  process.env.QA_ARTIFACTS ??
    join(root, "target", "qa", `${row}-${Date.now()}`),
);
const staged = await mkdtemp(join(tmpdir(), "agent-desktop-qa-"));
const name = `agent-desktop-qa-${crypto.randomUUID()}`;
let imageId: string | undefined;
let exitCode = 1;
let interrupted = false;
const stop = () => {
  interrupted = true;
  Bun.spawnSync(["podman", "rm", "-f", name], {
    stdout: "ignore",
    stderr: "ignore",
  });
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);

try {
  await mkdir(artifacts, { recursive: true });
  await cp(root, staged, {
    recursive: true,
    filter: (source) =>
      ![".git", "target", ".DS_Store"].includes(basename(source)),
  });
  const inspection = Bun.spawn(["podman", "image", "inspect", image], {
    stdout: "pipe",
    stderr: "inherit",
  });
  const imageInfo = await new Response(inspection.stdout).text();
  if ((await inspection.exited) !== 0)
    throw new Error(`Build ${image} with qa/containers/Containerfile first`);
  await writeFile(join(artifacts, "image.json"), imageInfo);
  const images: unknown = JSON.parse(imageInfo);
  const first: unknown = Array.isArray(images) ? images[0] : undefined;
  if (
    typeof first !== "object" ||
    first === null ||
    !("Id" in first) ||
    typeof first.Id !== "string"
  ) {
    throw new Error("Podman did not return an image ID");
  }
  imageId = first.Id;
  const cache = `agent-desktop-qa-${imageId.replace(/^sha256:/, "").slice(0, 12)}`;
  const args = [
    "podman",
    "run",
    "--rm",
    "--init",
    "--name",
    name,
    "--userns=keep-id",
    "--timeout=1200",
    "--shm-size=1g",
    "--memory=8g",
    "--cpus=4",
    "-v",
    `${staged}:/work:ro,Z`,
    "-v",
    `${cache}-build-${row}:/work/target`,
    "-v",
    `${cache}-cargo:/home/qa/.cargo`,
    "-v",
    `${artifacts}:/artifacts:rw,Z`,
    "-e",
    `QA_SUITE=${suite}`,
    "-e",
    `QA_DESKTOP=${desktop}`,
    "-e",
    `QA_PROTOCOL=${protocol}`,
    "--entrypoint",
    "/bin/bash",
    imageId,
    "/work/qa/containers/session.sh",
  ];
  // Glycin's nested bwrap sandbox needs this on SELinux hosts.
  if (desktop === "GNOME") args.splice(2, 0, "--security-opt", "label=disable");
  if (desktop === "KDE" && protocol === "wayland") {
    args.splice(
      2,
      0,
      "--device",
      process.env.QA_RENDER_DEVICE ?? "/dev/dri/renderD128",
      "--group-add",
      "keep-groups",
    );
  }
  if (interrupted) throw new Error("QA interrupted before container startup");
  console.log(`Running ${row}; artifacts: ${artifacts}`);
  const log = await open(join(artifacts, "run.log"), "w");
  try {
    const child = Bun.spawn(["bash", "-c", 'exec "$@" 2>&1', "qa", ...args], {
      stdout: "pipe",
      stderr: "inherit",
    });
    for await (const chunk of child.stdout) {
      await log.write(chunk);
      process.stdout.write(chunk);
    }
    exitCode = await child.exited;
  } finally {
    await log.close();
  }
} finally {
  Bun.spawnSync(["podman", "rm", "-f", name], {
    stdout: "ignore",
    stderr: "ignore",
  });
  process.off("SIGINT", stop);
  process.off("SIGTERM", stop);
  if (interrupted) exitCode = 130;
  await rm(staged, { recursive: true, force: true });
  await writeFile(
    join(artifacts, "result.json"),
    JSON.stringify(
      {
        suite,
        desktop,
        protocol,
        image: imageId ?? image,
        exitCode,
        status: exitCode === 0 ? "passed" : "failed",
      },
      null,
      2,
    ),
  );
}
process.exitCode = exitCode;
