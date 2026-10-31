import { spawn, type ChildProcess } from "node:child_process";

export function stringEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const out: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(env)) {
    if (typeof value === "string") out[key] = value;
  }
  return out;
}

/** Keeps the last lines so a stuck pipe cannot stall the child, and a failure can quote them. */
export function drain(child: ChildProcess): { text(): string } {
  let log = "";
  const take = (chunk: Buffer | string) => {
    log = (log + chunk.toString()).slice(-8000);
  };
  child.stdout?.on("data", take);
  child.stderr?.on("data", take);
  return { text: () => log };
}

export function stopProcessTree(child: ChildProcess): void {
  const pid = child.pid;
  if (pid === undefined) return;
  if (process.platform === "win32") {
    spawn("taskkill", ["/pid", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
    return;
  }
  try {
    process.kill(-pid, "SIGTERM");
  } catch {
    child.kill("SIGTERM");
  }
}

export function exited(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => {
    child.once("exit", () => resolve());
  });
}

export function spawnCommand(bin: string, args: string[], env: NodeJS.ProcessEnv, window: boolean): ChildProcess {
  return spawn(bin, args, {
    env: stringEnv(env),
    stdio: ["ignore", "pipe", "pipe"],
    detached: process.platform !== "win32",
    windowsHide: !window,
  });
}
