export type PlatformName = "windows" | "linux" | "macos";

/** A rectangle in pixels. `bounds` is relative to the recorded window. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** One tinyclient window. Clicks use the accessibility name the person sees. */
export interface Host {
  platform: PlatformName;
  find(name: string): Promise<boolean>;
  click(name: string): Promise<void>;
  bounds(name: string): Promise<Rect>;
  record(title: string, pcmPath: string, mp4Path: string): Promise<void>;
  close(): Promise<void>;
}

/** A client the scenario already started, so the driver attaches instead of launching a second copy. */
export interface HostTarget {
  pid: number;
  binary: string;
  env: NodeJS.ProcessEnv;
  output(): string;
}
