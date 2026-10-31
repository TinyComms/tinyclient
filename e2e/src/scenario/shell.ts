import type { Host } from "../host.js";
import type { ShellScenario } from "../catalog.js";

export async function runShell(scenario: ShellScenario, host: Host, timeoutMs: number): Promise<void> {
  for (const name of scenario.clicks) {
    await until(host, name, timeoutMs);
    await host.click(name);
  }
  for (const name of scenario.expect) {
    await until(host, name, timeoutMs);
  }
}

async function until(host: Host, name: string, timeoutMs: number): Promise<void> {
  const started = Date.now();
  for (;;) {
    if (await host.find(name)) return;
    if (Date.now() - started >= timeoutMs) {
      throw new Error(`timed out waiting for ${name}`);
    }
    await sleep(Math.min(50, Math.max(0, timeoutMs - (Date.now() - started))));
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
