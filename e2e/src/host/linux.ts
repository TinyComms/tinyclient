import type { Host, HostTarget, Rect } from "../host.js";
import { startAppium } from "./appium.js";
import { execCommand, requireCommand } from "./command.js";
import { appiumBin, ffmpegBin, present } from "./tools.js";
import { attach, driverHost } from "./desktop.js";
import { even, ffmpeg, muxArgs, RECORD_SECONDS } from "./ffmpeg.js";

/** Appium loads this driver for `atspi2`. Install it with `appium driver install --source=npm`. */
const LINUX_DRIVER = "@itsmeaj/appium-linux-driver";

export async function prepare(): Promise<{ stop(): Promise<void> }> {
  if (!(await present(ffmpegBin()))) await requireCommand("ffmpeg");
  if (!(await present(appiumBin()))) await requireCommand("appium");
  return startAppium();
}

export async function connect(target?: HostTarget): Promise<Host> {
  if (!target) throw new Error("tinyclient pid is required");
  let browser;
  try {
    browser = await attach({
      platformName: "Linux",
      "appium:automationName": "atspi2",
      "appium:appName": target.binary,
      "appium:newCommandTimeout": 180,
      // Keep the process the scenario started, so its scratch session and fixture stay in place.
      "appium:attachToRunningApp": true,
    } as WebdriverIO.Capabilities);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`appium session failed; install ${LINUX_DRIVER} (automationName atspi2): ${message}`);
  }
  return driverHost("linux", browser, async (_title, pcmPath, mp4Path) => {
    const rect = await windowRectForPid(target.pid);
    const display = process.env.DISPLAY && process.env.DISPLAY.length > 0 ? process.env.DISPLAY : ":0";
    await ffmpeg(x11grabArgs(rect, pcmPath, mp4Path, display));
  });
}

/** `wmctrl -lpG` lines: id, desktop, pid, x, y, width, height, host, title. */
export function rectForPid(listing: string, pid: number): Rect {
  for (const line of listing.split(/\r?\n/)) {
    const match = /^(\S+)\s+(\S+)\s+(\d+)\s+(-?\d+)\s+(-?\d+)\s+(\d+)\s+(\d+)\s+/.exec(line.trim());
    if (!match || Number(match[3]) !== pid) continue;
    const width = Number(match[6]);
    const height = Number(match[7]);
    if (width <= 0 || height <= 0) continue;
    return { x: Number(match[4]), y: Number(match[5]), width, height };
  }
  throw new Error(`no window for pid ${pid}`);
}

export function x11grabArgs(rect: Rect, pcmPath: string, mp4Path: string, display: string): string[] {
  const width = even(rect.width);
  const height = even(rect.height);
  const x = Math.max(0, Math.floor(rect.x));
  const y = Math.max(0, Math.floor(rect.y));
  const screen = display.length > 0 ? display : ":0";
  return [
    "-y",
    "-f", "x11grab",
    "-framerate", "30",
    "-t", RECORD_SECONDS,
    "-video_size", `${width}x${height}`,
    "-i", `${screen}+${x},${y}`,
    ...muxArgs(pcmPath, mp4Path),
  ];
}

async function windowRectForPid(pid: number): Promise<Rect> {
  await requireCommand("wmctrl");
  const { stdout } = await execCommand("wmctrl", ["-lpG"]);
  return rectForPid(stdout.toString("utf8"), pid);
}
