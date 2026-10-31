import { access, mkdir, mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import type { Host, HostTarget, PlatformName } from "./host.js";
import { requireCommand } from "./host/command.js";
import { grabFrame, mediaSeconds, pngSize, pngToRgb } from "./host/ffmpeg.js";
import { drain, spawnCommand, stopProcessTree } from "./proc.js";
import { runCall, type Runtime } from "./scenario/call.js";
import { startServer } from "./server.js";

export interface HostModule {
  prepare(): Promise<{ stop(): Promise<void> }>;
  connect(target?: HostTarget): Promise<Host>;
}

export function platformName(platform: string): PlatformName {
  switch (platform) {
    case "win32":
      return "windows";
    case "linux":
      return "linux";
    case "darwin":
      return "macos";
    default:
      throw new Error(`host module for ${platform} is missing`);
  }
}

export async function loadHost(platform: PlatformName, loader: () => Promise<HostModule>): Promise<HostModule> {
  let loaded: HostModule;
  try {
    loaded = await loader();
  } catch (error) {
    throw new Error(`host module for ${platform} is missing`, { cause: error });
  }
  if (typeof loaded.connect !== "function") {
    throw new Error(`host module for ${platform} is missing`);
  }
  return loaded;
}

function importHost(platform: PlatformName): Promise<HostModule> {
  switch (platform) {
    case "windows":
      return import("./host/windows.js");
    case "linux":
      return import("./host/linux.js");
    case "macos":
      return import("./host/macos.js");
  }
}

export function repoRoot(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
}

export async function main(): Promise<void> {
  const name = platformName(process.platform);
  const hostModule = await loadHost(name, () => importHost(name));
  const appium = await hostModule.prepare();
  try {
    const root = process.env.TINY_E2E_DATA ?? await mkdtemp(path.join(tmpdir(), "tiny-call-"));
    const dataDirA = path.join(root, "a");
    const dataDirB = path.join(root, "b");
    await mkdir(dataDirA, { recursive: true });
    await mkdir(dataDirB, { recursive: true });
    const timeout = Number(process.env.TINY_E2E_TIMEOUT_MS ?? "120000");
    await runCall({
      runtime: desktopRuntime(hostModule),
      fixtureDir: path.join(repoRoot(), "crates", "mediad", "tests", "fixtures", "call"),
      dataDirA,
      dataDirB,
      sessionA: process.env.TINY_E2E_SESSION_A ?? "",
      sessionBea: process.env.TINY_E2E_SESSION_BEA ?? "",
      windowTitle: "tinyclient",
      serverSource: process.env,
      timeoutMs: Number.isFinite(timeout) ? timeout : 120_000,
    });
  } finally {
    await appium.stop();
  }
}

function desktopRuntime(hostModule: HostModule): Runtime {
  let png = "";
  let mp4 = "";
  return {
    startServer,
    async startClient(env) {
      const bin = process.env.TINYCLIENT_BIN?.trim();
      if (!bin) throw new Error("the run needs the command: TINYCLIENT_BIN");
      await requireClient(bin);
      const dir = String(env.TINYCLIENT_DIR ?? "");
      await mkdir(dir, { recursive: true });
      const childEnv = { ...process.env, ...env };
      const child = spawnCommand(bin, [], childEnv, true);
      const output = drain(child);
      if (child.pid === undefined) throw new Error("the run needs the command: TINYCLIENT_BIN");
      const host = await hostModule.connect({
        pid: child.pid,
        binary: bin,
        env: childEnv,
        output: () => output.text(),
      });
      return {
        host,
        async stop() {
          try {
            await host.close();
          } catch {
            // The window may already be gone. The process tree still has to end.
          }
          stopProcessTree(child);
        },
      };
    },
    async grabPng(mp4Path, pngPath) {
      await grabFrame(mp4Path, pngPath);
      png = pngPath;
      mp4 = mp4Path;
    },
    async pngRgb() {
      const file = await readFile(png);
      const size = pngSize(file);
      return { ...size, pixels: await pngToRgb(png) };
    },
    async mp4Seconds() {
      return mediaSeconds(mp4);
    },
  };
}

async function requireClient(bin: string): Promise<void> {
  if (bin.includes("/") || bin.includes("\\")) {
    try {
      await access(bin);
    } catch {
      throw new Error(`the run needs the command: ${bin}`);
    }
    return;
  }
  await requireCommand(bin);
}

function startedAsCli(): boolean {
  const entry = process.argv[1];
  if (!entry) return false;
  return import.meta.url.toLowerCase() === pathToFileURL(path.resolve(entry)).href.toLowerCase();
}

if (startedAsCli()) {
  main().catch((error: unknown) => {
    const message = error instanceof Error ? error.message : String(error);
    process.stderr.write(`${message}\n`);
    process.exitCode = 1;
  });
}
