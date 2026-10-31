import type { Host, HostTarget } from "../host.js";
import { startAppium } from "./appium.js";
import { requireCommand } from "./command.js";
import { attach, driverHost } from "./desktop.js";
import { even, ffmpeg, muxArgs, RECORD_SECONDS } from "./ffmpeg.js";

/** Appium loads this driver for `atspi2`. Install it with `appium driver install --source=npm`. */
const LINUX_DRIVER = "@itsmeaj/appium-linux-driver";

export async function prepare(): Promise<{ stop(): Promise<void> }> {
  await requireCommand("ffmpeg");
  await requireCommand("appium");
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
    const rect = await browser.getWindowRect();
    const width = even(rect.width);
    const height = even(rect.height);
    const x = Math.max(0, Math.floor(rect.x));
    const y = Math.max(0, Math.floor(rect.y));
    const display = process.env.DISPLAY && process.env.DISPLAY.length > 0 ? process.env.DISPLAY : ":0";
    await ffmpeg([
      "-y",
      "-f", "x11grab",
      "-framerate", "30",
      "-t", RECORD_SECONDS,
      "-video_size", `${width}x${height}`,
      "-i", `${display}+${x},${y}`,
      ...muxArgs(pcmPath, mp4Path),
    ]);
  });
}
