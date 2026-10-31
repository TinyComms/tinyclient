import { execFile } from "node:child_process";

export function execCommand(
  command: string,
  args: string[],
  allowFailure = false,
): Promise<{ stdout: Buffer; stderr: string }> {
  return new Promise((resolve, reject) => {
    execFile(
      command,
      args,
      { encoding: "buffer", maxBuffer: 64 * 1024 * 1024, windowsHide: true },
      (error, stdout, stderr) => {
        const out = Buffer.isBuffer(stdout) ? stdout : Buffer.from(stdout ?? "");
        const err = Buffer.isBuffer(stderr) ? stderr.toString("utf8") : String(stderr ?? "");
        if (error && !allowFailure) {
          reject(new Error(`${command} ${args.join(" ")} failed: ${err || error.message}`));
          return;
        }
        resolve({ stdout: out, stderr: err });
      },
    );
  });
}

/** A missing tool fails the run. The message names the command the operator has to provide. */
export async function requireCommand(command: string): Promise<void> {
  const locator = process.platform === "win32" ? "where.exe" : "which";
  try {
    await execCommand(locator, [command]);
  } catch {
    throw new Error(`the run needs the command: ${command}`);
  }
}
