import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { Host, HostTarget } from "../host.js";
import { startAppium } from "./appium.js";
import { requireCommand } from "./command.js";
import { attach, driverHost } from "./desktop.js";
import { ffmpeg, muxArgs, RECORD_SECONDS } from "./ffmpeg.js";

const exec = promisify(execFile);

export async function prepare(): Promise<{ stop(): Promise<void> }> {
  await requireCommand("ffmpeg");
  await requireCommand("appium");
  return startAppium();
}

export async function connect(target?: HostTarget): Promise<Host> {
  if (!target) throw new Error("tinyclient pid is required");
  const handle = await topLevelWindow(target);
  const browser = await attach({
    platformName: "Windows",
    "appium:automationName": "Wincore",
    "appium:appTopLevelWindow": handle,
    "appium:newCommandTimeout": 180,
  });
  return driverHost("windows", browser, async (_title, pcmPath, mp4Path) => {
    await ffmpeg(gdigrabArgs(handle, pcmPath, mp4Path));
  });
}

/** gdigrab input for the window handle already resolved from this client's pid. */
export function gdigrabArgs(hwnd: string, pcmPath: string, mp4Path: string): string[] {
  return [
    "-y",
    "-f", "gdigrab",
    "-framerate", "30",
    "-t", RECORD_SECONDS,
    "-i", `hwnd=${hwnd}`,
    ...muxArgs(pcmPath, mp4Path),
  ];
}

async function topLevelWindow(target: HostTarget): Promise<string> {
  const deadline = Date.now() + 30_000;
  for (;;) {
    const handle = await mainWindowHandle(target.pid);
    if (handle) return handle;
    if (Date.now() >= deadline) {
      throw new Error(`tinyclient pid ${target.pid} opened no window\n${target.output()}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
}

async function mainWindowHandle(pid: number): Promise<string | undefined> {
  const { stdout } = await exec("powershell.exe", [
    "-NoProfile",
    "-Command",
    `(Get-Process -Id ${pid} -ErrorAction SilentlyContinue).MainWindowHandle`,
  ], { windowsHide: true });
  const value = Number(String(stdout).trim());
  if (!Number.isFinite(value) || value <= 0) return undefined;
  return `0x${value.toString(16)}`;
}
