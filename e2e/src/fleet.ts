import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { CALL, scenarioById } from "./catalog.js";
import type { PlatformName } from "./host.js";

export interface Desk {
  id: PlatformName;
  /** Project SSH alias. Absent when this process is already that desktop, or the alias is not in the project table. */
  ssh?: string;
}

/** Windows is `ssh winbuild`. The MacBook is `ssh mbp`. The Linux desk is `ssh tiny-linux`. */
export const DESKS: readonly Desk[] = [
  { id: "windows", ssh: "winbuild" },
  { id: "linux", ssh: "tiny-linux" },
  { id: "macos", ssh: "mbp" },
];

const FORWARDED = ["TINY_E2E_SESSION_A", "TINY_E2E_SESSION_BEA", "TINY_E2E_TIMEOUT_MS"] as const;

export interface FleetOptions {
  platform: string;
  repoRoot: string;
  env: NodeJS.ProcessEnv;
  runHere(command: string): Promise<void>;
}

export function localId(platform: string): PlatformName | undefined {
  switch (platform) {
    case "win32":
      return "windows";
    case "linux":
      return "linux";
    case "darwin":
      return "macos";
    default:
      return undefined;
  }
}

export function place(platform: string): { id: PlatformName; local: boolean; ssh?: string }[] {
  // The process that starts the fleet is the orchestrator. A desk is local only when
  // TINY_E2E_HERE names it, which is how a desk runs the command in its own session.
  void platform;
  const here = process.env.TINY_E2E_HERE;
  return DESKS.map((desk) => ({
    id: desk.id,
    local: desk.id === here,
    ssh: desk.id === here ? undefined : desk.ssh,
  }));
}

export function assertFleetCommand(command: string): void {
  if (command === "all" || command === "doctor" || command === CALL || scenarioById(command)) return;
  throw new Error(`unknown scenario ${command}`);
}

/** One `ssh <alias>` and the remote shell. No identity flags. */
export function sshArgv(alias: string, remoteCommand: string): string[] {
  return ["ssh", alias, remoteCommand];
}

export function tarArgs(root: string): string[] {
  return [
    "--exclude=target",
    "--exclude=node_modules",
    "--exclude=.git",
    "--exclude=e2e/dist",
    "--exclude=e2e/artifacts",
    "-cf", "-",
    "-C", root,
    ".",
  ];
}

export function posixQuote(value: string): string {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

function psQuote(value: string): string {
  return `'${value.replace(/'/g, "''")}'`;
}

/**
 * PowerShell for `ssh winbuild`. OpenSSH on that guest is session 0, so desk.ps1
 * schedules the command into the logged-on console and this script waits for it.
 * stdin is the checkout tar. stdout is the artifact tar.
 */
export function windowsRemoteShell(command: string, env: NodeJS.ProcessEnv): string {
  const lines = FORWARDED.filter((name) => typeof env[name] === "string" && env[name]!.length > 0).map(
    (name) => `$env:${name} = ${psQuote(env[name]!)}`,
  );
  const array = lines.length === 0 ? "@()" : `@(${lines.map((line) => psQuote(line)).join(",")})`;
  return [
    "$ErrorActionPreference = 'Continue'",
    "$root = Join-Path $env:USERPROFILE 'tiny-call-e2e'",
    "New-Item -ItemType Directory -Force -Path $root | Out-Null",
    "tar.exe -xf - -C $root",
    "if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }",
    "$art = Join-Path $root 'e2e\\artifacts'",
    "New-Item -ItemType Directory -Force -Path $art | Out-Null",
    "Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $art 'desk.exit')",
    `$lines = ${array}`,
    "if ($lines.Count -gt 0) { Set-Content -Path (Join-Path $root 'e2e\\desk-env.ps1') -Value $lines -Encoding ascii }",
    `& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'e2e\\scripts\\desk.ps1') ${psQuote(command)}`,
    "$code = $LASTEXITCODE",
    "tar.exe -cf - -C $art .",
    "exit $code",
  ].join("\n");
}

/** Extracts the checkout on stdin, runs the desk, and writes its artifact directory to stdout. */
export function remoteShell(command: string, env: NodeJS.ProcessEnv): string {
  const exports = FORWARDED
    .filter((name) => typeof env[name] === "string" && env[name]!.length > 0)
    .map((name) => `export ${name}=${posixQuote(env[name]!)}`);
  // `status` is read-only in zsh, which is the Mac login shell that receives this command.
  // stdin stays the checkout tar, so this is the remote command itself and not `sh -s`.
  return [
    "set -eu",
    'mkdir -p "$HOME/tiny-call-e2e"',
    'tar -xf - -C "$HOME/tiny-call-e2e"',
    'art="$HOME/tiny-call-e2e/e2e/artifacts"',
    'mkdir -p "$art"',
    ...exports,
    "set +e",
    `sh "$HOME/tiny-call-e2e/e2e/scripts/desk.sh" ${posixQuote(command)} >"$art/desk.log" 2>&1`,
    "desk_rc=$?",
    "set -e",
    'tar -cf - -C "$art" .',
    'exit "$desk_rc"',
  ].join("\n");
}

export async function runFleet(command: string, options: FleetOptions): Promise<void> {
  assertFleetCommand(command);
  const failures: string[] = [];
  for (const desk of place(options.platform)) {
    try {
      if (desk.local) {
        await runLocal(command, options.runHere);
        continue;
      }
      if (!desk.ssh) throw new Error(`${desk.id} has no project ssh alias`);
      const artifacts = path.join(options.repoRoot, "e2e", "artifacts", desk.id);
      await runRemote(desk.ssh, options.repoRoot, command, options.env, artifacts);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      failures.push(`${desk.id}: ${message}`);
    }
  }
  if (failures.length > 0) throw new Error(failures.join("\n"));
}

async function runLocal(command: string, runHere: (command: string) => Promise<void>): Promise<void> {
  const prior = process.exitCode;
  process.exitCode = undefined;
  let failed: unknown;
  try {
    await runHere(command);
  } catch (error) {
    failed = error;
  }
  const code = process.exitCode;
  process.exitCode = prior;
  if (failed) throw failed;
  if (typeof code === "number" && code !== 0) {
    throw new Error(`desktop command ${command} exited ${code}`);
  }
}

async function runRemote(
  alias: string,
  repoRoot: string,
  command: string,
  env: NodeJS.ProcessEnv,
  artifactDir: string,
): Promise<void> {
  await mkdir(artifactDir, { recursive: true });
  const tar = spawn("tar", tarArgs(repoRoot), { stdio: ["ignore", "pipe", "pipe"], windowsHide: true });
  const remote = alias === "winbuild" ? windowsRemoteShell(command, env) : remoteShell(command, env);
  const ssh = spawn("ssh", [alias, remote], {
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  tar.stdout?.pipe(ssh.stdin);
  const tarErr = collect(tar.stderr);
  const sshErr = collect(ssh.stderr);
  const sshOut = collect(ssh.stdout);
  const tarCode = await exitCode(tar);
  const sshCode = await exitCode(ssh);
  const bundle = Buffer.concat(sshOut.chunks);
  if (bundle.length > 0) {
    const file = path.join(tmpdir(), `tiny-e2e-${alias}.tar`);
    await writeFile(file, bundle);
    await mkdir(artifactDir, { recursive: true });
    await extract(file, artifactDir);
  }
  if (tarCode !== 0 || sshCode !== 0) {
    const detail = [tarErr.text(), sshErr.text()].filter((text) => text.length > 0).join("\n");
    throw new Error(`ssh ${alias} ${command} exited ${sshCode}\n${detail}`.trim());
  }
}

function extract(archive: string, dir: string): Promise<void> {
  const child = spawn("tar", ["-xf", archive, "-C", dir], { stdio: ["ignore", "ignore", "pipe"], windowsHide: true });
  const err = collect(child.stderr);
  return exitCode(child).then((code) => {
    if (code !== 0) throw new Error(`tar extract exited ${code}\n${err.text()}`.trim());
  });
}

function collect(stream: NodeJS.ReadableStream | null): { chunks: Buffer[]; text(): string } {
  const chunks: Buffer[] = [];
  stream?.on("data", (chunk: Buffer | string) => {
    chunks.push(typeof chunk === "string" ? Buffer.from(chunk) : chunk);
  });
  return {
    chunks,
    text() {
      return Buffer.concat(chunks).toString().slice(-4000);
    },
  };
}

function exitCode(child: ChildProcess): Promise<number> {
  return new Promise((resolve) => {
    child.once("close", (code) => resolve(code ?? 1));
  });
}
