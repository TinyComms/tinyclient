import assert from "node:assert/strict";
import { test } from "node:test";
import { loadHost, platformName, type HostModule } from "./cli.js";

const ready: HostModule = {
  async prepare() {
    return { async stop() {} };
  },
  async connect() {
    return {
      platform: "windows",
      async find() { return false; },
      async click() {},
      async bounds() { return { x: 0, y: 0, width: 0, height: 0 }; },
      async record() {},
      async close() {},
    };
  },
};

test("the cli maps the process platform onto a host", () => {
  assert.equal(platformName("win32"), "windows");
  assert.equal(platformName("linux"), "linux");
  assert.equal(platformName("darwin"), "macos");
});

test("an unknown platform is refused because its host module is missing", () => {
  assert.throws(() => platformName("freebsd"), /host module for freebsd is missing/);
});

test("a host module that does not load is refused", async () => {
  await assert.rejects(loadHost("windows", async () => {
    throw new Error("ENOENT");
  }), /host module for windows is missing/);
  await assert.rejects(loadHost("linux", async () => ({} as HostModule)), /host module for linux is missing/);
  const loaded = await loadHost("macos", async () => ready);
  assert.equal(typeof loaded.connect, "function");
});
