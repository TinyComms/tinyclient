import { access } from "node:fs/promises";
import path from "node:path";
import { appiumBin, ffmpegBin, present } from "./host/tools.js";

export interface Check {
  name: string;
  ok: boolean;
  detail: string;
}

export async function diagnose(env: NodeJS.ProcessEnv, fixtureDir: string): Promise<Check[]> {
  const checks: Check[] = [];
  checks.push(await bundled("ffmpeg", ffmpegBin()));
  checks.push(await bundled("appium", appiumBin()));
  checks.push(await file("TINYCLIENT_BIN", env.TINYCLIENT_BIN));
  checks.push(await file("CHATD_BIN", env.CHATD_BIN ?? sibling(env.TINYCLIENT_BIN, "chatd")));
  checks.push(await file("TINYSERVER_BIN", env.TINYSERVER_BIN));
  checks.push(await file("call fixtures", fixtureDir));
  checks.push(session("TINY_E2E_SESSION_A", env.TINY_E2E_SESSION_A));
  checks.push(session("TINY_E2E_SESSION_BEA", env.TINY_E2E_SESSION_BEA));
  return checks;
}

export function formatReport(checks: Check[]): string {
  const lines = checks.map((check) => `${check.ok ? "ok" : "missing"}  ${check.name}  ${check.detail}`);
  const ready = checks.every((check) => check.ok);
  lines.push(ready ? "ready" : "not ready");
  if (!ready) {
    lines.push("appium driver install wincore");
    lines.push("appium driver install mac2");
    lines.push("appium driver install --source=npm @itsmeaj/appium-linux-driver");
  }
  return lines.join("\n");
}

function session(name: string, value: string | undefined): Check {
  const present = typeof value === "string" && value.trim().length > 0;
  return { name, ok: present, detail: present ? "set" : "a saved Tiny session JSON" };
}

async function bundled(name: string, file: string): Promise<Check> {
  if (await present(file)) return { name, ok: true, detail: file };
  return { name, ok: false, detail: file };
}

async function file(name: string, value: string | undefined): Promise<Check> {
  if (!value || value.trim() === "") return { name, ok: false, detail: "not set" };
  try {
    await access(value);
    return { name, ok: true, detail: value };
  } catch {
    return { name, ok: false, detail: value };
  }
}

export function sibling(bin: string | undefined, name: string): string | undefined {
  if (!bin) return undefined;
  const exe = process.platform === "win32" && !name.endsWith(".exe") ? `${name}.exe` : name;
  return path.join(path.dirname(bin), exe);
}
