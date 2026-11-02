import assert from "node:assert/strict";
import { test } from "node:test";
import { place, posixQuote, remoteShell, sshArgv, tarArgs, windowsRemoteShell } from "./fleet.js";

test("the mac desk is one plain ssh mbp", () => {
  const mac = place("win32").find((desk) => desk.id === "macos");
  assert.ok(mac);
  assert.equal(mac.local, false);
  assert.equal(mac.ssh, "mbp");
  assert.deepEqual(sshArgv("mbp", "true"), ["ssh", "mbp", "true"]);
});

test("the windows desk is ssh winbuild and the linux desk is ssh tiny-linux", () => {
  const placed = place("win32");
  const windows = placed.find((desk) => desk.id === "windows");
  assert.equal(windows?.local, false);
  assert.equal(windows?.ssh, "winbuild");
  const linux = placed.find((desk) => desk.id === "linux");
  assert.equal(linux?.local, false);
  assert.equal(linux?.ssh, "tiny-linux");
  assert.deepEqual(sshArgv("tiny-linux", "true"), ["ssh", "tiny-linux", "true"]);
});

test("the windows remote shell extracts the checkout and waits for the console task", () => {
  const shell = windowsRemoteShell("doctor", { TINY_E2E_SESSION_A: "a'b" });
  assert.equal(shell.includes("-i "), false);
  assert.equal(shell.includes("IdentityFile"), false);
  assert.match(shell, /tar\.exe -xf - -C \$root/);
  assert.match(shell, /desk\.ps1'\) 'doctor'/);
  assert.match(shell, /TINY_E2E_SESSION_A = ''a''''b'''/);
  assert.match(shell, /tar\.exe -cf - -C \$art \./);
});

test("the remote shell extracts the checkout and returns the artifact tar", () => {
  const shell = remoteShell("doctor", {});
  assert.equal(shell.includes("-i "), false);
  assert.equal(shell.includes("IdentityFile"), false);
  assert.equal(shell.includes("IdentityAgent"), false);
  assert.match(shell, /tar -xf - -C "\$HOME\/tiny-call-e2e"/);
  assert.match(shell, /desk\.sh" 'doctor'/);
  assert.match(shell, /tar -cf - -C "\$art" \./);
});

test("a session value is single-quoted for the remote shell", () => {
  const shell = remoteShell("all", { TINY_E2E_SESSION_A: "a'b" });
  assert.match(shell, /export TINY_E2E_SESSION_A='a'"'"'b'/);
  assert.equal(posixQuote("a'b"), `'a'"'"'b'`);
});

test("the checkout tar leaves out build output and keys are not an archive flag", () => {
  const args = tarArgs("C:\\src\\tiny");
  assert.ok(args.includes("--exclude=node_modules"));
  assert.ok(args.includes("--exclude=target"));
  assert.ok(args.includes("--exclude=.git"));
  assert.equal(args.includes("-i"), false);
  assert.deepEqual(args.slice(-3), ["-C", "C:\\src\\tiny", "."]);
});
