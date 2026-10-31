export type PlatformName = "windows" | "linux" | "macos";

/** One tinyclient window. Clicks use the accessibility name the person sees. */
export interface Host {
  platform: PlatformName;
  find(name: string): Promise<boolean>;
  click(name: string): Promise<void>;
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
