import assert from "node:assert/strict";
import { test } from "node:test";
import { rectForPid, x11grabArgs } from "./linux.js";
import { avfoundationArgs, macWindowScript, parseMacRect } from "./macos.js";
import { gdigrabArgs } from "./windows.js";

test("windows records that client's hwnd and not the shared title", () => {
  const ada = gdigrabArgs("0x10", "a.pcm", "a.mp4").join(" ");
  const bea = gdigrabArgs("0x20", "b.pcm", "b.mp4").join(" ");
  assert.match(ada, /hwnd=0x10/);
  assert.match(bea, /hwnd=0x20/);
  assert.equal(ada.includes("title="), false);
  assert.equal(bea.includes("title="), false);
  assert.notEqual(ada, bea);
});

test("macos asks for that pid's window and crops to its rectangle", () => {
  const script = macWindowScript(4242);
  assert.match(script, /unix id is 4242/);
  assert.equal(script.includes("title="), false);
  assert.equal(script.toLowerCase().includes("tinyclient"), false);
  const ada = parseMacRect("10, 20, 800, 600");
  const bea = parseMacRect("900, 40, 640, 480");
  const adaGrab = avfoundationArgs(ada, "a.pcm", "a.mp4", "1:none").join(" ");
  const beaGrab = avfoundationArgs(bea, "b.pcm", "b.mp4", "1:none").join(" ");
  assert.match(adaGrab, /crop=800:600:10:20/);
  assert.match(beaGrab, /crop=640:480:900:40/);
  assert.equal(adaGrab.includes("title="), false);
  assert.notEqual(adaGrab, beaGrab);
});

test("linux records the rectangle of that pid when two windows share a title", () => {
  const listing = [
    "0x100 0 111 0 0 800 600 host tinyclient",
    "0x200 0 222 400 50 640 480 host tinyclient",
  ].join("\n");
  const ada = rectForPid(listing, 111);
  const bea = rectForPid(listing, 222);
  assert.deepEqual(ada, { x: 0, y: 0, width: 800, height: 600 });
  assert.deepEqual(bea, { x: 400, y: 50, width: 640, height: 480 });
  const adaGrab = x11grabArgs(ada, "a.pcm", "a.mp4", ":0").join(" ");
  const beaGrab = x11grabArgs(bea, "b.pcm", "b.mp4", ":0").join(" ");
  assert.match(adaGrab, /800x600/);
  assert.match(adaGrab, /:0\+0,0/);
  assert.match(beaGrab, /640x480/);
  assert.match(beaGrab, /:0\+400,50/);
  assert.equal(adaGrab.includes("title="), false);
  assert.notEqual(adaGrab, beaGrab);
});
