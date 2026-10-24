// Run with: node crates/login/tests/discord-probe.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const { runInNewContext } = require('node:vm');

const probe = readFileSync(new URL('../src/discord-probe.js', `file://${__filename}`), 'utf8');

async function check(main, frame, expected, expectFrame, status = 200) {
  let cleared = false;
  let redirected = null;
  let requests = 0;
  let appended = 0;
  let removed = 0;
  const storage = value => ({ getItem: key => {
    assert.equal(key, 'token');
    return value;
  }, clear: () => { cleared = true; } });
  const blocked = () => { throw new Error('storage unavailable'); };
  const window = {};
  Object.defineProperty(window, 'localStorage', {
    get: main === 'blocked' ? blocked : () => storage(main),
  });
  const document = {
    documentElement: { appendChild: () => { appended++; } },
    createElement: tag => {
      assert.equal(tag, 'iframe');
      const contentWindow = {};
      Object.defineProperty(contentWindow, 'localStorage', {
        get: frame === 'blocked' ? blocked : () => storage(frame),
      });
      return { style: {}, contentWindow, remove: () => { removed++; } };
    },
  };
  window.location = { replace: url => { redirected = url; } };
  const context = { window, document, AbortController, setTimeout, clearTimeout,
    fetch: async (url, options) => {
      requests++;
      assert.equal(url, 'https://discord.com/api/v9/users/@me');
      assert.equal(options.headers.Authorization, expected?.token || 'revoked-token');
      if (status === 'offline') throw new Error('offline');
      return { ok: status === 200, status };
    },
  };
  const first = runInNewContext(probe, context);
  assert.equal(first, null, 'a stored token must be checked before it is returned');
  await new Promise(resolve => setImmediate(resolve));
  const result = status === 200 ? runInNewContext(probe, context) : first;
  assert.deepEqual(result === null ? null : JSON.parse(result), expected);
  assert.equal(appended, expectFrame ? (status === 200 ? 2 : 1) : 0);
  assert.equal(removed, appended, 'temporary frames must be removed');
  assert.equal(cleared, status === 401);
  assert.equal(redirected, status === 401 ? 'https://discord.com/login' : null);
  if (expected || status !== 200) assert.equal(requests, 1);
}

test('reads the token directly without creating a frame', () =>
  check('"direct-token"', null, { token: 'direct-token' }, false));
test('uses a frame when main storage is hidden', () =>
  check('blocked', '"frame-token"', { token: 'frame-token' }, true));
test('uses a frame when main storage has no token', () =>
  check(null, '"frame-token"', { token: 'frame-token' }, true));
test('cleans up a frame even when its storage is blocked', () =>
  check('blocked', 'blocked', null, true));
test('returns null when neither store holds a token', () =>
  check(null, null, null, true));
for (const value of ['""', '{', 'null', '42', '{}']) {
  test(`rejects invalid token value ${value}`, () =>
    check(value, null, null, false));
}

test('a revoked token resets the browser login instead of closing the window', () =>
  check('"revoked-token"', null, null, false, 401));
test('a server error keeps the browser login for another attempt', () =>
  check('"revoked-token"', null, null, false, 503));
test('a connection failure keeps the browser login for another attempt', () =>
  check('"revoked-token"', null, null, false, 'offline'));
