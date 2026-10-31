import assert from "node:assert/strict";
import { test } from "node:test";
import { averageRgb, averagesApart, cropRgb, pcmRms, rmsAboveFloor } from "./oracle.js";

test("pcm rms is the root mean square of little-endian samples", () => {
  const pcm = Buffer.alloc(4);
  pcm.writeInt16LE(6, 0);
  pcm.writeInt16LE(8, 2);
  assert.ok(Math.abs(pcmRms(pcm) - Math.sqrt(50)) < 1e-9);
});

test("silence is under the speech floor and a steady tone is over it", () => {
  const silence = Buffer.alloc(8);
  const tone = Buffer.alloc(8);
  tone.writeInt16LE(1000, 0);
  tone.writeInt16LE(1000, 2);
  tone.writeInt16LE(1000, 4);
  tone.writeInt16LE(1000, 6);
  assert.equal(rmsAboveFloor(pcmRms(silence)), false);
  assert.equal(pcmRms(silence), 0);
  assert.equal(rmsAboveFloor(pcmRms(tone)), true);
});

test("rgb average of the four quadrants is the midpoint", () => {
  const pixels = new Uint8Array([
    255, 0, 0,
    0, 255, 0,
    0, 0, 255,
    255, 255, 255,
  ]);
  assert.deepEqual(averageRgb(pixels), { r: 127.5, g: 127.5, b: 127.5 });
});

test("a crop is the Call picture, not the average of the whole window", () => {
  const window = new Uint8Array([
    255, 0, 0,
    255, 255, 255,
    0, 255, 0,
    0, 0, 255,
  ]);
  assert.deepEqual(averageRgb(window), { r: 127.5, g: 127.5, b: 127.5 });
  const picture = cropRgb(window, 4, { x: 0, y: 0, width: 1, height: 1 });
  assert.deepEqual(averageRgb(picture), { r: 255, g: 0, b: 0 });
  assert.equal(averagesApart(averageRgb(picture), averageRgb(window)), true);
});

test("colour averages more than twenty apart tell bars from quadrants", () => {
  const quadrants = { r: 127.5, g: 127.5, b: 127.5 };
  const bars = { r: 109.714, g: 109.714, b: 109.714 };
  const same = { r: 127.5, g: 128, b: 127.5 };
  assert.equal(averagesApart(quadrants, bars), true);
  assert.equal(averagesApart(quadrants, same), false);
  assert.equal(averagesApart({ r: 0, g: 0, b: 0 }, { r: 20, g: 0, b: 0 }), false);
  assert.equal(averagesApart({ r: 0, g: 0, b: 0 }, { r: 21, g: 0, b: 0 }), true);
});
