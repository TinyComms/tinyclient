import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { Host, HostTarget, Rect } from "../host.js";
import { stringEnv } from "../proc.js";
import { startAppium } from "./appium.js";
import { requireCommand } from "./command.js";
import { appiumBin, ffmpegBin, present } from "./tools.js";
import { attach, driverHost } from "./desktop.js";
import { even, ffmpeg, muxArgs, RECORD_SECONDS } from "./ffmpeg.js";

const exec = promisify(execFile);

const BUNDLE_ID = "dev.tinyclient.app";

export async function prepare(): Promise<{ stop(): Promise<void> }> {
  if (!(await present(ffmpegBin()))) await requireCommand("ffmpeg");
  if (!(await present(appiumBin()))) await requireCommand("appium");
  return startAppium();
}

export async function connect(target?: HostTarget): Promise<Host> {
  if (!target) throw new Error("tinyclient pid is required");
  const pathToApp = appPath(target.binary);
  const browser = await attach({
    platformName: "Mac",
    "appium:automationName": "mac2",
    "appium:bundleId": BUNDLE_ID,
    ...(pathToApp ? { "appium:appPath": pathToApp } : {}),
    "appium:noReset": true,
    "appium:skipAppKill": true,
    "appium:environment": stringEnv(target.env),
    "appium:newCommandTimeout": 180,
  } as WebdriverIO.Capabilities);
  return driverHost("macos", browser, async (_title, pcmPath, mp4Path) => {
    const rect = await windowRectForPid(target.pid);
    await ffmpeg(avfoundationArgs(rect, pcmPath, mp4Path));
  });
}

/** AppleScript for the first window of this pid. The shared title is not a selector. */
export function macWindowScript(pid: number): string {
  return [
    'tell application "System Events"',
    `set proc to first process whose unix id is ${pid}`,
    "set w to first window of proc",
    "set p to position of w",
    "set s to size of w",
    'return (item 1 of p as text) & "," & (item 2 of p as text) & "," & (item 1 of s as text) & "," & (item 2 of s as text)',
    "end tell",
  ].join("\n");
}

export function parseMacRect(text: string): Rect {
  const [x, y, width, height] = text.trim().split(",").map((part) => Number(part.trim()));
  if (![x, y, width, height].every((value) => Number.isFinite(value))) {
    throw new Error(`could not read the window rectangle: ${text}`);
  }
  return { x: x ?? 0, y: y ?? 0, width: width ?? 0, height: height ?? 0 };
}

export function avfoundationArgs(
  rect: Rect,
  pcmPath: string,
  mp4Path: string,
  input = process.env.TINY_E2E_AVFOUNDATION ?? "1:none",
): string[] {
  const width = even(rect.width);
  const height = even(rect.height);
  const x = Math.max(0, Math.floor(rect.x));
  const y = Math.max(0, Math.floor(rect.y));
  return [
    "-y",
    "-f", "avfoundation",
    "-framerate", "30",
    "-t", RECORD_SECONDS,
    "-i", input,
    ...muxArgs(pcmPath, mp4Path, [`crop=${width}:${height}:${x}:${y}`]),
  ];
}

async function windowRectForPid(pid: number): Promise<Rect> {
  const { stdout } = await exec("osascript", ["-e", macWindowScript(pid)]);
  return parseMacRect(String(stdout));
}

function appPath(binary: string): string | undefined {
  const marker = ".app/Contents/MacOS/";
  const at = binary.replaceAll("\\", "/").indexOf(marker);
  if (at === -1) return binary.endsWith(".app") ? binary : undefined;
  return binary.slice(0, at + 4);
}
