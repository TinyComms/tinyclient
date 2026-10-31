import assert from "node:assert/strict";
import { test } from "node:test";
import { CALL, SHELL, scenarioById } from "../catalog.js";
import type { Host } from "../host.js";
import { runShell } from "./shell.js";

function scripted(present: string[]): { host: Host; trace: string[] } {
  const trace: string[] = [];
  const host: Host = {
    platform: "windows",
    async find(name) {
      trace.push(`find ${name}`);
      return present.includes(name);
    },
    async click(name) {
      trace.push(`click ${name}`);
    },
    async bounds() {
      return { x: 0, y: 0, width: 10, height: 10 };
    },
    async record() {},
    async close() {},
  };
  return { host, trace };
}

test("the catalog names every demo surface and the call", () => {
  const ids = SHELL.map((scenario) => scenario.id);
  assert.deepEqual(ids, ["inbox", "people", "discord", "slack", "telegram", "teams", "signal", "settings"]);
  const discord = scenarioById("discord");
  assert.ok(discord && discord !== CALL && discord.clicks[0] === "Discord");
  assert.equal(scenarioById(CALL), CALL);
  assert.equal(scenarioById("missing"), undefined);
});

test("a shell scenario clicks in order and then requires what the screen must show", async () => {
  const discord = SHELL.find((scenario) => scenario.id === "discord");
  assert.ok(discord);
  const { host, trace } = scripted(["Discord", "Moss & Lichen", "#showcase"]);
  await runShell(discord, host, 1_000);
  assert.deepEqual(trace, [
    "find Discord",
    "click Discord",
    "find Moss & Lichen",
    "click Moss & Lichen",
    "find #showcase",
    "click #showcase",
    "find #showcase",
  ]);
});

test("a missing row fails the scenario instead of moving on", async () => {
  const inbox = SHELL.find((scenario) => scenario.id === "inbox");
  assert.ok(inbox);
  const { host } = scripted(["Inbox"]);
  await assert.rejects(runShell(inbox, host, 40), /timed out waiting for Yusuf Demirci/);
});
