import { access } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));

/** The ffmpeg this package installed, or the one on PATH. */
export function ffmpegBin(): string {
  try {
    const bundled = require("ffmpeg-static") as string | null;
    if (bundled) return bundled;
  } catch {
    // The package is not installed. PATH is the remaining place.
  }
  return "ffmpeg";
}

/** The Appium this package installed, or the one on PATH. */
export function appiumBin(): string {
  const local = path.join(here, "..", "..", "node_modules", "appium", "index.js");
  return local;
}

export async function present(file: string): Promise<boolean> {
  try {
    await access(file);
    return true;
  } catch {
    return false;
  }
}
