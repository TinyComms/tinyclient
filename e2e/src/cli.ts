import { access, mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { CALL, SHELL, scenarioById, type ShellScenario } from "./catalog.js";
import { diagnose, formatReport, sibling } from "./doctor.js";
import type { Host, HostTarget, PlatformName } from "./host.js";
import { requireCommand } from "./host/command.js";
import { grabFrame, mediaSeconds, pngSize, pngToRgb } from "./host/ffmpeg.js";
import { drain, spawnCommand, stopProcessTree } from "./proc.js";
import { runCall, type Runtime } from "./scenario/call.js";
import { runShell } from "./scenario/shell.js";
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

function fixtureDir(): string {
  return path.join(repoRoot(), "crates", "mediad", "tests", "fixtures", "call");
}

function timeoutMs(): number {
  const timeout = Number(process.env.TINY_E2E_TIMEOUT_MS ?? "120000");
  return Number.isFinite(timeout) ? timeout : 120_000;
}

export async function main(argv: string[] = process.argv): Promise<void> {
  const command = argv[2] ?? "all";
  if (command === "doctor") {
    const checks = await diagnose(process.env, fixtureDir());
    process.stdout.write(`${formatReport(checks)}\n`);
    if (checks.some((check) => !check.ok)) process.exitCode = 1;
    return;
  }
  if (command !== "all" && command !== CALL && !scenarioById(command)) {
    throw new Error(`unknown scenario ${command}`);
  }
  const name = platformName(process.platform);
  const hostModule = await loadHost(name, () => importHost(name));
  const appium = await hostModule.prepare();
  try {
    const root = process.env.TINY_E2E_DATA ?? await mkdtemp(path.join(tmpdir(), "tiny-e2e-"));
    await mkdir(root, { recursive: true });
    if (command === "all" || command === CALL) {
      await runOneCall(hostModule, root);
    }
    const shells = command === "all" ? SHELL : command === CALL ? [] : [scenarioById(command) as ShellScenario];
    for (const scenario of shells) {
      await runOneShell(hostModule, scenario, path.join(root, scenario.id));
    }
  } finally {
    await appium.stop();
  }
}

async function runOneCall(hostModule: HostModule, root: string): Promise<void> {
  const dataDirA = path.join(root, "a");
  const dataDirB = path.join(root, "b");
  await mkdir(dataDirA, { recursive: true });
  await mkdir(dataDirB, { recursive: true });
  await runCall({
    runtime: desktopRuntime(hostModule),
    fixtureDir: fixtureDir(),
    dataDirA,
    dataDirB,
    sessionA: process.env.TINY_E2E_SESSION_A ?? "",
    sessionBea: process.env.TINY_E2E_SESSION_BEA ?? "",
    windowTitle: "tinyclient",
    serverSource: process.env,
    timeoutMs: timeoutMs(),
  });
}

async function runOneShell(hostModule: HostModule, scenario: ShellScenario, dir: string): Promise<void> {
  await mkdir(dir, { recursive: true });
  const chatdBin = process.env.CHATD_BIN ?? sibling(process.env.TINYCLIENT_BIN, "chatd");
  if (!chatdBin) throw new Error("the run needs the command: CHATD_BIN");
  await requireClient(chatdBin);
  const clientBin = process.env.TINYCLIENT_BIN?.trim();
  if (!clientBin) throw new Error("the run needs the command: TINYCLIENT_BIN");
  await requireClient(clientBin);
  const env = { ...process.env, TINYCLIENT_DIR: dir };
  const chatd = spawnCommand(chatdBin, ["--seed-demo"], env, true);
  const chatdOut = drain(chatd);
  const client = spawnCommand(clientBin, [], env, true);
  const clientOut = drain(client);
  if (client.pid === undefined) throw new Error("the run needs the command: TINYCLIENT_BIN");
  try {
    const host = await hostModule.connect({
      pid: client.pid,
      binary: clientBin,
      env,
      output: () => `${chatdOut.text()}\n${clientOut.text()}`,
    });
    try {
      await runShell(scenario, host, timeoutMs());
      const pcm = path.join(dir, "silence.pcm");
      const mp4 = path.join(dir, `${scenario.id}.mp4`);
      await writeFile(pcm, Buffer.alloc(48_000 * 2 * 2 * 2));
      await host.record("", pcm, mp4);
      if (!((await mediaSeconds(mp4)) > 1)) throw new Error(`${scenario.id} mp4 must be longer than one second`);
    } finally {
      await host.close().catch(() => undefined);
    }
  } finally {
    stopProcessTree(client);
    stopProcessTree(chatd);
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
