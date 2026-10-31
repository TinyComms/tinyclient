import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import type { Host, Rect } from "../host.js";
import { runCall, type Runtime } from "./call.js";

const FIXTURE = path.join("crates", "mediad", "tests", "fixtures", "call");
const SESSION_A = JSON.stringify({
  account: "ada",
  token: "token-a",
  base_url: "http://127.0.0.1:8080",
});
const SESSION_BEA = JSON.stringify({
  account: "bea",
  token: "token-b",
  base_url: "http://127.0.0.1:8080",
});
const QUADRANTS = new Uint8Array([
  255, 0, 0,
  0, 255, 0,
  0, 0, 255,
  255, 255, 255,
]);
const RED = new Uint8Array([255, 0, 0, 255, 0, 0]);
const ROOM = 'room POST http://127.0.0.1:8080/v1/rooms Bearer token-a {"name":"Proof"}';

interface Frame {
  width: number;
  height: number;
  pixels: Uint8Array;
}

interface Trace {
  steps: string[];
  serverEnv: NodeJS.ProcessEnv | undefined;
}

function host(who: "a" | "b", steps: string[], answers: Map<string, boolean>, picture: Rect): Host {
  return {
    platform: "windows",
    async find(name) {
      steps.push(`find ${who} ${name}`);
      return answers.get(`${who}:${name}`) ?? false;
    },
    async click(name) {
      steps.push(`click ${who} ${name}`);
    },
    async bounds(name) {
      steps.push(`bounds ${who} ${name}`);
      return picture;
    },
    async record(title, pcmPath, mp4Path) {
      steps.push(`record ${who} ${title} ${pcmPath} ${mp4Path}`);
    },
    async close() {},
  };
}

function harness(options: {
  answers?: Map<string, boolean>;
  frame?: Frame;
  seconds?: number;
  writeMp4?: boolean;
  pcm?: Buffer;
  picture?: Rect;
}): { runtime: Runtime; trace: Trace } {
  const steps: string[] = [];
  const trace: Trace = { steps, serverEnv: undefined };
  const answers = options.answers ?? new Map<string, boolean>([
    ["a:Proof", true],
    ["b:Proof", true],
    ["a:Join channel", true],
    ["b:Join channel", true],
    ["a:Leave the call", true],
    ["b:Leave the call", true],
    ["a:fixture", true],
    ["b:fixture", true],
    ["b:Watch", true],
    ["b:Stop watching", true],
  ]);
  const frame = options.frame ?? { width: 4, height: 1, pixels: QUADRANTS };
  const picture = options.picture ?? { x: 0, y: 0, width: frame.width, height: frame.height };
  let grabbed = false;
  const runtime: Runtime = {
    async startServer(env) {
      trace.serverEnv = env;
      steps.push("server");
      return { async stop() { steps.push("stop server"); } };
    },
    async startClient(env) {
      const dir = String(env.TINYCLIENT_DIR);
      const who = dir.endsWith(`${path.sep}a`) || dir.endsWith("/a") ? "a" : "b";
      steps.push(
        `client ${who} ${env.TINYCLIENT_DIR} ${env.TINYCLIENT_CALL_FIXTURE} ${env.TINYCLIENT_SESSION_TINY} ${env.TINYCLIENT_MEDIAD_SINK}`,
      );
      return {
        who,
        host: host(who, steps, answers, picture),
        async stop() { steps.push(`stop ${who}`); },
      };
    },
    async grabPng(mp4, png) {
      grabbed = true;
      steps.push(`grab ${mp4} ${png}`);
    },
    async pngRgb() {
      if (!grabbed) {
        throw new Error("read the png before ffmpeg grabbed it");
      }
      return frame;
    },
    async mp4Seconds() {
      return options.seconds ?? 2;
    },
  };
  return { runtime, trace };
}

async function layout(): Promise<{ a: string; b: string }> {
  const root = await mkdtemp(path.join(tmpdir(), "tiny-call-"));
  const a = path.join(root, "a");
  const b = path.join(root, "b");
  return { a, b };
}

async function run(partial: {
  answers?: Map<string, boolean>;
  frame?: Frame;
  seconds?: number;
  pcm?: Buffer;
  sessionA?: string;
  sessionBea?: string;
  serverSource?: NodeJS.ProcessEnv;
  timeoutMs?: number;
  writeMp4?: boolean;
  picture?: Rect;
} = {}): Promise<Trace> {
  const dirs = await layout();
  const { runtime, trace } = harness(partial);
  const pcm = partial.pcm ?? (() => {
    const loud = Buffer.alloc(8);
    loud.writeInt16LE(1000, 0);
    loud.writeInt16LE(1000, 2);
    loud.writeInt16LE(1000, 4);
    loud.writeInt16LE(1000, 6);
    return loud;
  })();
  const sink = path.join(dirs.b, "played.pcm");
  const { mkdir } = await import("node:fs/promises");
  await mkdir(dirs.b, { recursive: true });
  await writeFile(sink, pcm);
  if (partial.writeMp4 !== false) {
    await writeFile(path.join(dirs.b, "remote.mp4"), Buffer.from("mp4"));
  }
  const previous = globalThis.fetch;
  globalThis.fetch = async (input, init) => {
    const headers = new Headers(init?.headers);
    trace.steps.push(`room ${init?.method ?? "GET"} ${String(input)} ${headers.get("authorization") ?? ""} ${String(init?.body ?? "")}`);
    return new Response("{}", { status: 201 });
  };
  try {
    await runCall({
      runtime,
      fixtureDir: FIXTURE,
      dataDirA: dirs.a,
      dataDirB: dirs.b,
      sessionA: partial.sessionA ?? SESSION_A,
      sessionBea: partial.sessionBea ?? SESSION_BEA,
      windowTitle: "tinyclient",
      serverSource: partial.serverSource ?? {},
      timeoutMs: partial.timeoutMs ?? 1000,
    });
  } finally {
    globalThis.fetch = previous;
  }
  return trace;
}

test("the call joins both clients, sends the pictures, and records the remote window", async () => {
  const trace = await run();
  const server = trace.serverEnv ?? {};
  assert.equal(server.TINYSERVER_PUBLIC_IP, "127.0.0.1");
  assert.equal(server.TINYSERVER_MEDIA_PORT, "50000");
  assert.equal(server.DATABASE_URL, "postgres://app_runtime:app_runtime@127.0.0.1:54329/tinyserver?sslmode=disable");
  assert.equal(
    server.DATABASE_URL_MIGRATE,
    "postgres://app_owner:app_owner@127.0.0.1:54329/tinyserver?sslmode=disable",
  );
  assert.equal(trace.steps[1], ROOM);
  const clients = trace.steps.filter((step) => step.startsWith("client "));
  assert.equal(clients.length, 2);
  assert.match(clients[0] ?? "", new RegExp(`client a .+ ${escapeRe(FIXTURE)} ${escapeRe(SESSION_A)} `));
  assert.match(clients[1] ?? "", new RegExp(`client b .+ ${escapeRe(FIXTURE)} ${escapeRe(SESSION_BEA)} `));
  assert.match(clients[0] ?? "", /played\.pcm$/);
  assert.match(clients[1] ?? "", /played\.pcm$/);
  assert.notEqual(clients[0], clients[1]);
  const record = trace.steps.find((step) => step.startsWith("record "));
  assert.match(record ?? "", /^record b tinyclient /);
  assert.match(record ?? "", /played\.pcm /);
  assert.match(record ?? "", /remote\.mp4$/);
  assert.ok(trace.steps.some((step) => step.startsWith("grab ") && step.endsWith("frame.png")));
  assert.deepEqual(trace.steps.filter((step) => !step.startsWith("client ") && step !== "server" && !step.startsWith("room ") && !step.startsWith("grab ") && !step.startsWith("stop")), [
    "find a Proof",
    "find b Proof",
    "click a Proof",
    "click b Proof",
    "find a Join channel",
    "find b Join channel",
    "click a Join channel",
    "click b Join channel",
    "find a Leave the call",
    "find b Leave the call",
    "find a fixture",
    "find b fixture",
    "click a Turn camera on",
    "click a Share your screen",
    "find b Stop watching",
    "find b Watch",
    "click b Watch",
    "find b Stop watching",
    "bounds b Call picture",
    "record b tinyclient " + sinkOf(trace, "b") + " " + mp4Of(trace),
  ]);
  assert.deepEqual(trace.steps.slice(0, 4).map((step) => step.split(" ")[0]), ["server", "room", "client", "client"]);
  assert.deepEqual(trace.steps.slice(-3), ["stop a", "stop b", "stop server"]);
});

test("a microphone that is not the fixture is selected before the camera", async () => {
  const answers = new Map<string, boolean>([
    ["a:Proof", true],
    ["b:Proof", true],
    ["a:Join channel", true],
    ["b:Join channel", true],
    ["a:Leave the call", true],
    ["b:Leave the call", true],
    ["a:fixture", false],
    ["b:fixture", false],
    ["b:Stop watching", true],
  ]);
  const trace = await run({ answers });
  assert.deepEqual(trace.steps.filter((step) => !step.startsWith("client ") && (step.includes("fixture") || step.includes("Microphone") || step.includes("camera") || step.includes("Share"))), [
    "find a fixture",
    "click a Microphone and speaker",
    "click a Microphone",
    "click a fixture",
    "find b fixture",
    "click b Microphone and speaker",
    "click b Microphone",
    "click b fixture",
    "click a Turn camera on",
    "click a Share your screen",
  ]);
  assert.equal(trace.steps.filter((step) => step === "click b Watch").length, 0);
});

test("an operator database url is the one the server process gets", async () => {
  const trace = await run({
    serverSource: {
      TINYSERVER_PUBLIC_IP: "10.0.0.8",
      DATABASE_URL: "postgres://app_runtime:secret@127.0.0.1:54329/tinyserver?sslmode=disable",
    },
  });
  assert.equal(trace.serverEnv?.TINYSERVER_PUBLIC_IP, "10.0.0.8");
  assert.equal(trace.serverEnv?.TINYSERVER_MEDIA_PORT, "50000");
  assert.equal(trace.serverEnv?.DATABASE_URL, "postgres://app_runtime:secret@127.0.0.1:54329/tinyserver?sslmode=disable");
});

test("a missing session does not start a process or invent a sign-in", async () => {
  await assert.rejects(run({ sessionA: "" }), /TINY_E2E_SESSION_A/);
  await assert.rejects(run({ sessionBea: " " }), /TINY_E2E_SESSION_BEA/);
});

test("a session without a token or base_url fails with that fact", async () => {
  await assert.rejects(run({ sessionA: '{"base_url":"http://127.0.0.1:8080"}' }), /TINY_E2E_SESSION_A has no token/);
  await assert.rejects(run({ sessionA: '{"token":"token-a"}' }), /TINY_E2E_SESSION_A has no base_url/);
  await assert.rejects(run({ sessionBea: '{"base_url":"http://127.0.0.1:8080","token":""}' }), /TINY_E2E_SESSION_BEA has no token/);
});

test("the remote sink must carry the speech clip", async () => {
  await assert.rejects(run({ pcm: Buffer.alloc(4800) }), /speech/);
});

test("the recorded frame must be the share picture", async () => {
  await assert.rejects(run({ frame: { width: 2, height: 1, pixels: RED } }), /share/);
});

test("the share is the Call picture crop, not the whole window", async () => {
  const windowAveragingToShare = new Uint8Array([
    255, 0, 0,
    255, 255, 255,
    0, 255, 0,
    0, 0, 255,
  ]);
  await assert.rejects(run({
    frame: { width: 4, height: 1, pixels: windowAveragingToShare },
    picture: { x: 0, y: 0, width: 1, height: 1 },
  }), /share/);

  const shellAndQuadrants = new Uint8Array([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    255, 0, 0,
    0, 255, 0,
    0, 0, 255,
    255, 255, 255,
  ]);
  await run({
    frame: { width: 8, height: 1, pixels: shellAndQuadrants },
    picture: { x: 4, y: 0, width: 4, height: 1 },
  });
});

test("the mp4 must exist and be longer than one second", async () => {
  await assert.rejects(run({ seconds: 1 }), /one second/);
  await assert.rejects(run({ writeMp4: false, seconds: 3 }), /mp4/);
});

test("a client that never shows Join is stopped", async () => {
  const answers = new Map<string, boolean>([
    ["a:Proof", true],
    ["b:Proof", true],
  ]);
  await assert.rejects(run({ answers, timeoutMs: 0 }), /Join channel/);
});

function escapeRe(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function sinkOf(trace: Trace, who: "a" | "b"): string {
  const line = trace.steps.find((step) => step.startsWith(`client ${who} `));
  const parts = line?.split(" ") ?? [];
  return parts[parts.length - 1] ?? "";
}

function mp4Of(trace: Trace): string {
  const line = trace.steps.find((step) => step.startsWith("record "));
  const parts = line?.split(" ") ?? [];
  return parts[parts.length - 1] ?? "";
}
