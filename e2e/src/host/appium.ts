import { spawn } from "node:child_process";
import net from "node:net";
import { drain, exited, stopProcessTree } from "../proc.js";
import { appiumBin, present } from "./tools.js";

export const APPIUM_HOST = "127.0.0.1";
export const APPIUM_PORT = 4723;

function portOpen(port: number): Promise<boolean> {
  return new Promise((resolve) => {
    const socket = net.connect({ host: APPIUM_HOST, port });
    const done = (open: boolean) => {
      socket.removeAllListeners();
      socket.destroy();
      resolve(open);
    };
    socket.once("connect", () => done(true));
    socket.once("error", () => done(false));
  });
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Starts Appium when nothing is already listening. A missing `appium` binary is the caller's error. */
export async function startAppium(): Promise<{ stop(): Promise<void> }> {
  if (await portOpen(APPIUM_PORT)) {
    return { async stop() {} };
  }
  const entry = appiumBin();
  if (!(await present(entry))) throw new Error("the run needs the command: appium");
  const child = spawn(process.execPath, [entry, "--address", APPIUM_HOST, "--port", String(APPIUM_PORT)], {
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  const output = drain(child);
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`the run needs the command: appium\n${output.text()}`);
    }
    if (await portOpen(APPIUM_PORT)) {
      return {
        async stop() {
          stopProcessTree(child);
          await exited(child);
        },
      };
    }
    await sleep(200);
  }
  stopProcessTree(child);
  throw new Error(`the run needs the command: appium\n${output.text()}`);
}
