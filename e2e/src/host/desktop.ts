import { remote, type Browser } from "webdriverio";
import type { Host, PlatformName } from "../host.js";
import { APPIUM_HOST, APPIUM_PORT } from "./appium.js";

const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";

export async function attach(capabilities: WebdriverIO.Capabilities): Promise<Browser> {
  return remote({
    protocol: "http",
    hostname: APPIUM_HOST,
    port: APPIUM_PORT,
    path: "/",
    logLevel: "silent",
    automationProtocol: "webdriver",
    connectionRetryCount: 2,
    connectionRetryTimeout: 30_000,
    capabilities: { ...capabilities, webSocketUrl: false },
  });
}

async function waitFor(browser: Browser, name: string): Promise<string> {
  const deadline = Date.now() + 10_000;
  for (;;) {
    const id = await elementId(browser, name);
    if (id) return id;
    if (Date.now() >= deadline) throw new Error(`no control named ${name}`);
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

async function elementId(browser: Browser, name: string): Promise<string | undefined> {
  for (const using of ["name", "accessibility id"]) {
    try {
      const found = await browser.findElements(using, name);
      const id = found[0]?.[ELEMENT];
      if (id) return id;
    } catch {
      // This driver may not implement that locator. The other one can still hit.
    }
  }
  return undefined;
}

export function driverHost(platform: PlatformName, browser: Browser, record: Host["record"]): Host {
  return {
    platform,
    async find(name) {
      return (await elementId(browser, name)) !== undefined;
    },
    async click(name) {
      const id = await waitFor(browser, name);
      await browser.elementClick(id);
    },
    async bounds(name) {
      const id = await waitFor(browser, name);
      const element = await browser.getElementRect(id);
      const window = await browser.getWindowRect();
      return {
        x: Math.round(element.x - window.x),
        y: Math.round(element.y - window.y),
        width: Math.round(element.width),
        height: Math.round(element.height),
      };
    },
    record,
    async close() {
      await browser.deleteSession();
    },
  };
}
