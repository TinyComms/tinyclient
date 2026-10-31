import type { Host, HostTarget } from "../host.js";
import { stringEnv } from "../proc.js";
import { startAppium } from "./appium.js";
import { requireCommand } from "./command.js";
import { attach, driverHost } from "./desktop.js";
import { even, ffmpeg, muxArgs, RECORD_SECONDS } from "./ffmpeg.js";

const BUNDLE_ID = "dev.tinyclient.app";

export async function prepare(): Promise<{ stop(): Promise<void> }> {
  await requireCommand("ffmpeg");
  await requireCommand("appium");
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
    const rect = await browser.getWindowRect();
    const width = even(rect.width);
    const height = even(rect.height);
    const x = Math.max(0, Math.floor(rect.x));
    const y = Math.max(0, Math.floor(rect.y));
    const input = process.env.TINY_E2E_AVFOUNDATION ?? "1:none";
    await ffmpeg([
      "-y",
      "-f", "avfoundation",
      "-framerate", "30",
      "-t", RECORD_SECONDS,
      "-i", input,
      ...muxArgs(pcmPath, mp4Path, [`crop=${width}:${height}:${x}:${y}`]),
    ]);
  });
}

function appPath(binary: string): string | undefined {
  const marker = ".app/Contents/MacOS/";
  const at = binary.replaceAll("\\", "/").indexOf(marker);
  if (at === -1) return binary.endsWith(".app") ? binary : undefined;
  return binary.slice(0, at + 4);
}
