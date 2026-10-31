import { access } from "node:fs/promises";
import type { ChildProcess } from "node:child_process";
import { requireCommand } from "./host/command.js";
import { drain, exited, spawnCommand, stopProcessTree, stringEnv } from "./proc.js";

const HEALTH_WAIT_MS = 90_000;

/**
 * Spawns `TINYSERVER_BIN`. `migrate` applies the schema, then `serve` listens.
 * The scenario supplies the call environment; everything else comes from the operator's process.
 */
export async function startServer(env: NodeJS.ProcessEnv): Promise<{ stop(): Promise<void> }> {
  const bin = process.env.TINYSERVER_BIN?.trim();
  if (!bin) throw new Error("the run needs the command: TINYSERVER_BIN");
  await requireBinary(bin);
  const childEnv = stringEnv({ ...process.env, ...env });
  await runToCompletion(bin, ["migrate"], childEnv);
  const child = spawnCommand(bin, ["serve"], childEnv, false);
  const output = drain(child);
  try {
    await waitUntilHealthy(childEnv.BIND ?? "127.0.0.1:8080", child, output);
  } catch (error) {
    stopProcessTree(child);
    throw error;
  }
  return {
    async stop() {
      stopProcessTree(child);
      await exited(child);
    },
  };
}

async function requireBinary(bin: string): Promise<void> {
  if (!bin.includes("/") && !bin.includes("\\")) {
    await requireCommand(bin);
    return;
  }
  try {
    await access(bin);
  } catch {
    throw new Error(`the run needs the command: ${bin}`);
  }
}

async function runToCompletion(bin: string, args: string[], env: NodeJS.ProcessEnv): Promise<void> {
  const child = spawnCommand(bin, args, env, false);
  const output = drain(child);
  const code = await new Promise<number | null>((resolve) => {
    child.once("exit", (status) => resolve(status));
  });
  if (code !== 0) {
    throw new Error(`${bin} ${args.join(" ")} failed (${code ?? "signal"}): ${output.text()}`);
  }
}

async function waitUntilHealthy(
  bind: string,
  child: ChildProcess,
  output: { text(): string },
): Promise<void> {
  const origin = bind.includes("://") ? bind : `http://${bind}`;
  const deadline = Date.now() + HEALTH_WAIT_MS;
  let last = "";
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`tinyserver serve exited ${child.exitCode}: ${output.text() || last}`);
    }
    try {
      const response = await fetch(`${origin}/health`);
      if (response.ok) return;
      last = `health ${response.status}`;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`tinyserver serve did not become healthy: ${output.text() || last}`);
}
