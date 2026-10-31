import { access, mkdir, readFile } from "node:fs/promises";
import path from "node:path";
import type { Host, Rect } from "../host.js";
import { averageRgb, averagesApart, cropRgb, pcmRms, rmsAboveFloor, type Rgb } from "../oracle.js";

/**
 * Four equal quadrants (red, green, blue, white) average to the midpoint.
 * The colour-bar camera clip averages near 109.714, which `averagesApart` separates from this.
 */
const SHARE: Rgb = { r: 127.5, g: 127.5, b: 127.5 };
const CAMERA_BARS: Rgb = { r: 109.714, g: 109.714, b: 109.714 };

/**
 * Scratch chatd reads `TINYCLIENT_SESSION_` plus the network label in uppercase
 * (`session.rs`). Tiny's label is "Tiny server", so the process also receives that name.
 * `TINYCLIENT_SESSION_TINY` is the name this scenario is required to set. No other sign-in is added.
 */
const CHATD_SESSION_TINY = "TINYCLIENT_SESSION_TINY SERVER";

const SERVER_DEFAULTS = {
  TINYSERVER_PUBLIC_IP: "127.0.0.1",
  TINYSERVER_MEDIA_PORT: "50000",
  DATABASE_URL: "postgres://app_runtime:app_runtime@127.0.0.1:54329/tinyserver?sslmode=disable",
  DATABASE_URL_MIGRATE: "postgres://app_owner:app_owner@127.0.0.1:54329/tinyserver?sslmode=disable",
} as const;

export interface ClientHandle {
  who?: string;
  host: Host;
  stop(): Promise<void>;
}

export interface Runtime {
  startServer(env: NodeJS.ProcessEnv): Promise<{ stop(): Promise<void> }>;
  startClient(env: NodeJS.ProcessEnv): Promise<ClientHandle>;
  grabPng(mp4: string, png: string): Promise<void>;
  pngRgb(): Promise<{ width: number; height: number; pixels: Uint8Array }>;
  mp4Seconds(): Promise<number>;
}

export interface CallOptions {
  runtime: Runtime;
  fixtureDir: string;
  dataDirA: string;
  dataDirB: string;
  sessionA: string;
  sessionBea: string;
  windowTitle: string;
  serverSource: NodeJS.ProcessEnv;
  timeoutMs: number;
}

interface SavedSession {
  token: string;
  baseUrl: string;
}

export async function runCall(options: CallOptions): Promise<void> {
  const adaSession = readSession(options.sessionA, "TINY_E2E_SESSION_A");
  readSession(options.sessionBea, "TINY_E2E_SESSION_BEA");
  await mkdir(options.dataDirA, { recursive: true });
  await mkdir(options.dataDirB, { recursive: true });

  const server = await options.runtime.startServer(serverEnv(options.serverSource));
  let ada: ClientHandle | undefined;
  let bea: ClientHandle | undefined;
  try {
    await createRoom(adaSession);
    ada = await options.runtime.startClient(clientEnv(options.dataDirA, options.fixtureDir, options.sessionA));
    bea = await options.runtime.startClient(clientEnv(options.dataDirB, options.fixtureDir, options.sessionBea));
    await joinRoom(ada.host, bea.host, options.timeoutMs);
    await selectFixture(ada.host);
    await selectFixture(bea.host);
    await ada.host.click("Turn camera on");
    await ada.host.click("Share your screen");
    await receiveShare(bea.host, options.timeoutMs);

    const sink = path.join(options.dataDirB, "played.pcm");
    const mp4 = path.join(options.dataDirB, "remote.mp4");
    const png = path.join(options.dataDirB, "frame.png");
    const picture = await bea.host.bounds("Call picture");
    await bea.host.record(options.windowTitle, sink, mp4);
    await options.runtime.grabPng(mp4, png);
    await assertCall(options.runtime, sink, mp4, picture);
  } finally {
    await stop(ada);
    await stop(bea);
    await stop(server);
  }
}

function readSession(value: string, name: string): SavedSession {
  if (value.trim() === "") throw new Error(`${name} is required`);
  let parsed: { token?: unknown; base_url?: unknown };
  try {
    parsed = JSON.parse(value) as { token?: unknown; base_url?: unknown };
  } catch {
    throw new Error(`${name} is not session JSON`);
  }
  if (typeof parsed.token !== "string" || parsed.token.trim() === "") {
    throw new Error(`${name} has no token`);
  }
  if (typeof parsed.base_url !== "string" || parsed.base_url.trim() === "") {
    throw new Error(`${name} has no base_url`);
  }
  return { token: parsed.token, baseUrl: parsed.base_url.replace(/\/$/, "") };
}

async function createRoom(session: SavedSession): Promise<void> {
  const url = `${session.baseUrl}/v1/rooms`;
  const response = await fetch(url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${session.token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ name: "Proof" }),
  });
  if (!response.ok) {
    throw new Error(`POST ${url} failed: ${response.status}`);
  }
}

function serverEnv(source: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {};
  for (const key of Object.keys(SERVER_DEFAULTS) as (keyof typeof SERVER_DEFAULTS)[]) {
    const given = source[key];
    env[key] = given === undefined || given === "" ? SERVER_DEFAULTS[key] : given;
  }
  return env;
}

function clientEnv(dir: string, fixtureDir: string, session: string): NodeJS.ProcessEnv {
  return {
    TINYCLIENT_DIR: dir,
    TINYCLIENT_CALL_FIXTURE: fixtureDir,
    TINYCLIENT_SESSION_TINY: session,
    [CHATD_SESSION_TINY]: session,
    TINYCLIENT_MEDIAD_SINK: path.join(dir, "played.pcm"),
  };
}

async function joinRoom(ada: Host, bea: Host, timeoutMs: number): Promise<void> {
  await until(ada, "Proof", timeoutMs);
  await until(bea, "Proof", timeoutMs);
  await ada.click("Proof");
  await bea.click("Proof");
  await until(ada, "Join channel", timeoutMs);
  await until(bea, "Join channel", timeoutMs);
  await ada.click("Join channel");
  await bea.click("Join channel");
  await until(ada, "Leave the call", timeoutMs);
  await until(bea, "Leave the call", timeoutMs);
}

/** The fixture input is the default while the directory is set. Pick it when a machine still shows another mic. */
async function selectFixture(host: Host): Promise<void> {
  if (await host.find("fixture")) return;
  await host.click("Microphone and speaker");
  await host.click("Microphone");
  await host.click("fixture");
}

/**
 * The remote strip says Watch until the share is on screen, then Stop watching.
 * A driver that already reports both still opens Watch, which is what the scenario trace records.
 */
async function receiveShare(host: Host, timeoutMs: number): Promise<void> {
  const started = Date.now();
  let watching = await host.find("Stop watching");
  let offered = await host.find("Watch");
  while (!watching && !offered) {
    if (Date.now() - started >= timeoutMs) {
      throw new Error("timed out waiting for the share");
    }
    await sleep(Math.min(50, Math.max(0, timeoutMs - (Date.now() - started))));
    watching = await host.find("Stop watching");
    offered = await host.find("Watch");
  }
  if (offered) {
    await host.click("Watch");
    await until(host, "Stop watching", timeoutMs);
  }
}

async function assertCall(runtime: Runtime, sink: string, mp4: string, picture: Rect): Promise<void> {
  let pcm: Buffer;
  try {
    pcm = await readFile(sink);
  } catch {
    throw new Error("remote PCM is not the speech clip");
  }
  if (!rmsAboveFloor(pcmRms(pcm))) {
    throw new Error("remote PCM is not the speech clip");
  }
  const image = await runtime.pngRgb();
  const frame = averageRgb(cropRgb(image.pixels, image.width, picture));
  if (!averagesApart(frame, CAMERA_BARS) || averagesApart(frame, SHARE)) {
    throw new Error("recorded frame is not the share picture");
  }
  try {
    await access(mp4);
  } catch {
    throw new Error("remote mp4 is missing");
  }
  const seconds = await runtime.mp4Seconds();
  if (!(seconds > 1)) throw new Error("mp4 must be longer than one second");
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

async function stop(handle: { stop(): Promise<void> } | undefined): Promise<void> {
  if (!handle) return;
  try {
    await handle.stop();
  } catch {
    // The failure that left the run is the one to report. Still stop the other processes.
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
