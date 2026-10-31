import { execCommand } from "./command.js";
import { ffmpegBin } from "./tools.js";

/** Long enough that a successful grab is past the one-second pass rule. */
export const RECORD_SECONDS = "2";

export function even(value: number): number {
  const whole = Math.max(2, Math.floor(value));
  return whole - (whole % 2);
}

/** Audio mux shared by every host. The video input is already on the command. */
export function muxArgs(pcmPath: string, mp4Path: string, videoFilters: string[] = []): string[] {
  const filters = [...videoFilters, "scale=trunc(iw/2)*2:trunc(ih/2)*2"];
  return [
    "-f", "s16le",
    "-ar", "48000",
    "-ac", "2",
    "-i", pcmPath,
    "-vf", filters.join(","),
    "-t", RECORD_SECONDS,
    "-c:v", "libx264",
    "-pix_fmt", "yuv420p",
    "-c:a", "aac",
    mp4Path,
  ];
}

export async function ffmpeg(args: string[], allowFailure = false): Promise<{ stdout: Buffer; stderr: string }> {
  return execCommand(ffmpegBin(), args, allowFailure);
}

export async function grabFrame(mp4Path: string, pngPath: string): Promise<void> {
  await ffmpeg(["-y", "-i", mp4Path, "-frames:v", "1", pngPath]);
}

export function pngSize(png: Buffer): { width: number; height: number } {
  if (png.length < 24 || png.toString("ascii", 12, 16) !== "IHDR") {
    throw new Error("png has no IHDR");
  }
  return { width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
}

export async function pngToRgb(pngPath: string): Promise<Uint8Array> {
  const { stdout } = await ffmpeg(["-i", pngPath, "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
  return new Uint8Array(stdout);
}

export async function mediaSeconds(file: string): Promise<number> {
  const { stderr } = await ffmpeg(["-i", file], true);
  const match = /Duration:\s*(\d+):(\d+):(\d+(?:\.\d+)?)/.exec(stderr);
  if (!match) throw new Error(`could not read duration of ${file}`);
  const hours = Number(match[1]);
  const minutes = Number(match[2]);
  const seconds = Number(match[3]);
  return hours * 3600 + minutes * 60 + seconds;
}
