import assert from "node:assert/strict";
import path from "node:path";
import { test } from "node:test";
import { formatReport, sibling, type Check } from "./doctor.js";

test("a missing piece is named, and the three driver installs follow", () => {
  const checks: Check[] = [
    { name: "ffmpeg", ok: false, detail: "not on PATH" },
    { name: "TINYCLIENT_BIN", ok: true, detail: "C:\\tinyclient.exe" },
  ];
  const text = formatReport(checks);
  assert.match(text, /missing {2}ffmpeg {2}not on PATH/);
  assert.match(text, /ok {2}TINYCLIENT_BIN/);
  assert.match(text, /not ready/);
  assert.match(text, /appium driver install wincore/);
  assert.match(text, /appium driver install mac2/);
  assert.match(text, /appium-linux-driver/);
});

test("a ready machine does not print driver installs", () => {
  const text = formatReport([{ name: "ffmpeg", ok: true, detail: "on PATH" }]);
  assert.match(text, /^ready$/m);
  assert.equal(text.includes("appium driver install"), false);
});

test("chatd sits beside the client binary", () => {
  const bin = path.join("C:\\apps", process.platform === "win32" ? "tinyclient.exe" : "tinyclient");
  const chatd = sibling(bin, "chatd");
  assert.ok(chatd);
  assert.equal(path.dirname(chatd), path.dirname(bin));
  assert.equal(path.basename(chatd), process.platform === "win32" ? "chatd.exe" : "chatd");
  assert.equal(sibling(undefined, "chatd"), undefined);
});
