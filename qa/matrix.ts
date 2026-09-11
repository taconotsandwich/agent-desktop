import { cp, mkdir, mkdtemp, rm, open, writeFile } from "node:fs/promises";
import { resolve, join, basename } from "node:path";
import { tmpdir } from "node:os";

type Row = {
  name: string;
  desktop: "KDE" | "GNOME";
  protocol: "wayland" | "x11";
  image: string;
};
const rows: Row[] = [
  {
    name: "fedora43-kde-wayland",
    desktop: "KDE",
    protocol: "wayland",
    image: "localhost/agent-desktop-qa:fedora43",
  },
  {
    name: "fedora43-kde-x11",
    desktop: "KDE",
    protocol: "x11",
    image: "localhost/agent-desktop-qa:fedora43",
  },
  {
    name: "fedora43-gnome-wayland",
    desktop: "GNOME",
    protocol: "wayland",
    image: "localhost/agent-desktop-qa:fedora43",
  },
];
const requested = process.argv.slice(2);
const selected = requested.length
  ? rows.filter((row) => requested.includes(row.name))
  : rows;
if (selected.length !== requested.length && requested.length)
  throw new Error("Unknown matrix row");
const root = resolve(import.meta.dir, "..");
const output = join(root, "target", "qa", `matrix-${Date.now()}`);
await mkdir(output, { recursive: true });
const staged = await mkdtemp(join(tmpdir(), "agent-desktop-matrix-"));
const reports: {
  name: string;
  status: string;
  exitCode: number;
  image: string;
  artifacts: string;
}[] = [];
try {
  await cp(root, staged, {
    recursive: true,
    filter: (source) =>
      ![".git", "target", ".DS_Store"].includes(basename(source)),
  });
  for (const row of selected) {
    const artifacts = join(output, row.name);
    await mkdir(artifacts, { recursive: true });
    const inspection = Bun.spawn(["podman", "image", "inspect", row.image], {
      stdout: "pipe",
      stderr: "inherit",
    });
    const imageInfo = await new Response(inspection.stdout).text();
    if ((await inspection.exited) !== 0)
      throw new Error(
        `Build ${row.image} with qa/containers/Containerfile first`,
      );
    await writeFile(join(artifacts, "image.json"), imageInfo);
    const images: unknown = JSON.parse(imageInfo);
    const first: unknown = Array.isArray(images) ? images[0] : undefined;
    if (
      typeof first !== "object" ||
      first === null ||
      !("Id" in first) ||
      typeof first.Id !== "string"
    )
      throw new Error("Podman did not return an image ID");
    const imageId = first.Id;
    const log = await open(join(artifacts, "run.log"), "w");
    const name = `agent-desktop-qa-${row.name}-${Date.now()}`;
    console.log(`Running ${row.name}; artifacts: ${artifacts}`);
    const args = [
      "podman",
      "run",
      "--rm",
      "--init",
      "--name",
      name,
      "--userns=keep-id",
      "--shm-size=1g",
      "--memory=8g",
      "--cpus=4",
      "-v",
      `${staged}:/work:ro,Z`,
      "-v",
      "agent-desktop-qa-build-f43:/work/target",
      "-v",
      "agent-desktop-qa-cargo-f43:/home/qa/.cargo",
      "-v",
      `${artifacts}:/artifacts:rw,Z`,
      "-e",
      `QA_DESKTOP=${row.desktop}`,
      "-e",
      `QA_PROTOCOL=${row.protocol}`,
      "--entrypoint",
      "/bin/bash",
      imageId,
      "/work/qa/containers/session.sh",
    ];
    // Glycin's nested bwrap sandbox is blocked by container_t on Fedora.
    // This disposable GNOME row keeps Podman's user namespace and seccomp filter.
    if (row.desktop === "GNOME")
      args.splice(2, 0, "--security-opt", "label=disable");
    if (row.desktop === "KDE" && row.protocol === "wayland")
      args.splice(
        2,
        0,
        "--device",
        "/dev/dri/renderD128",
        "--group-add",
        "keep-groups",
      );
    const child = Bun.spawn(args, { stdout: log.fd, stderr: log.fd });
    const timer = setTimeout(() => {
      Bun.spawnSync(["podman", "stop", "--time", "5", name], {
        stdout: "ignore",
        stderr: "ignore",
      });
    }, 1_200_000);
    let code: number;
    try {
      code = await child.exited;
    } finally {
      clearTimeout(timer);
      Bun.spawnSync(["podman", "rm", "-f", name], {
        stdout: "ignore",
        stderr: "ignore",
      });
      await log.close();
    }
    reports.push({
      name: row.name,
      status: code === 0 ? "passed" : "failed",
      exitCode: code,
      image: imageId,
      artifacts,
    });
    await writeFile(
      join(output, "results.json"),
      JSON.stringify(reports, null, 2),
    );
    console.log(`${row.name}: ${code === 0 ? "passed" : "failed"}`);
  }
} finally {
  await rm(staged, { recursive: true, force: true });
}
console.log(`Results: ${join(output, "results.json")}`);
process.exitCode = reports.every((report) => report.status === "passed")
  ? 0
  : 1;
