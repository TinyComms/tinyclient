// Run with: node crates/login/tests/discord-probe.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const { runInNewContext } = require('node:vm');

const source = readFileSync(new URL('../src/common.rs', `file://${__filename}`), 'utf8');
const discord = source.split('pub const DISCORD: Site = Site {')[1].split('\n};')[0];
const probe = JSON.parse(discord.match(/probe: (".*"),/)[1]);

function check(main, frame, expected, expectFrame) {
  let appended = 0;
  let removed = 0;
  const storage = value => ({ getItem: key => {
    assert.equal(key, 'token');
    return value;
  } });
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
  const result = runInNewContext(probe, { window, document });
  assert.deepEqual(result === null ? null : JSON.parse(result), expected);
  assert.equal(appended, expectFrame ? 1 : 0);
  assert.equal(removed, appended, 'temporary frames must be removed');
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
