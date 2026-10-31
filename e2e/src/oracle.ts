/** One pixel's channels, each the mean of that channel across the buffer. */
export interface Rgb {
  r: number;
  g: number;
  b: number;
}

/**
 * Root mean square of little-endian stereo `i16` samples.
 * A trailing odd byte is not a sample.
 */
export function pcmRms(pcm: Buffer): number {
  const samples = Math.floor(pcm.length / 2);
  if (samples === 0) return 0;
  let sum = 0;
  for (let i = 0; i < samples; i++) {
    const sample = pcm.readInt16LE(i * 2);
    sum += sample * sample;
  }
  return Math.sqrt(sum / samples);
}

/**
 * Silence is 0. The committed speech clip sits near 3200, and a steady tone of 1000 is speech.
 * Anything at or under this floor is not that clip.
 */
const SPEECH_FLOOR = 200;

export function rmsAboveFloor(rms: number): boolean {
  return rms > SPEECH_FLOOR;
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The pixels inside `bounds` of a tightly packed RGB image of `width` pixels per row. */
export function cropRgb(pixels: Uint8Array, width: number, bounds: Rect): Uint8Array {
  if (width <= 0) return new Uint8Array();
  const height = Math.floor(pixels.length / 3 / width);
  const x0 = clamp(Math.floor(bounds.x), 0, width);
  const y0 = clamp(Math.floor(bounds.y), 0, height);
  const x1 = clamp(x0 + Math.floor(bounds.width), x0, width);
  const y1 = clamp(y0 + Math.floor(bounds.height), y0, height);
  const out = new Uint8Array((x1 - x0) * (y1 - y0) * 3);
  let n = 0;
  for (let y = y0; y < y1; y++) {
    for (let x = x0; x < x1; x++) {
      const i = (y * width + x) * 3;
      out[n++] = pixels[i] ?? 0;
      out[n++] = pixels[i + 1] ?? 0;
      out[n++] = pixels[i + 2] ?? 0;
    }
  }
  return out;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** Mean of tightly packed RGB triples. A trailing partial pixel is ignored. */
export function averageRgb(pixels: Uint8Array): Rgb {
  const count = Math.floor(pixels.length / 3);
  if (count === 0) return { r: 0, g: 0, b: 0 };
  let r = 0;
  let g = 0;
  let b = 0;
  for (let i = 0; i < count; i++) {
    r += pixels[i * 3] ?? 0;
    g += pixels[i * 3 + 1] ?? 0;
    b += pixels[i * 3 + 2] ?? 0;
  }
  return { r: r / count, g: g / count, b: b / count };
}

/** Euclidean distance in RGB. Colour bars and the four-quadrant share sit just past 20 apart. */
const APART = 20;

export function averagesApart(left: Rgb, right: Rgb): boolean {
  return Math.hypot(left.r - right.r, left.g - right.g, left.b - right.b) > APART;
}
