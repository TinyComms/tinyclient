const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { parseInvite } = require('./invite.js');
const pack = (origin, token = 'tok-en_1') => `tc1.${Buffer.from(origin).toString('hex')}.${token}`;

assert.deepEqual(parseInvite(pack('https://Chat.Example:8443')), {
  origin: 'https://chat.example:8443', host: 'chat.example:8443', insecure: false,
  payload: pack('https://Chat.Example:8443')
});
assert.equal(parseInvite(pack('http://[::1]:8080')).insecure, true);
for (const origin of ['ftp://host', 'https://user@host', 'https://host/path', 'https://host?x', 'https://host#x', 'https://host:0', 'https://host:99999', 'https://host\\evil', 'https://host:', 'https://host.']) {
  assert.equal(parseInvite(pack(origin)), null, origin);
}
for (const payload of ['', 'tc1.zz.token', 'tc1.61.', pack('https://host', 'x'.repeat(257)), 'a'.repeat(2049), pack('https://host', 'bad.token')]) {
  assert.equal(parseInvite(payload), null);
}

// Exercise the page script without network access or a browser installation.
function render(payload) {
  const elements = {};
  const events = {};
  for (const id of ['invite-error', 'invite-ready', 'server-host', 'insecure-warning', 'open-tiny', 'invite-code', 'copy-link', 'copy-status']) {
    elements[id] = { hidden: true, addEventListener() {} };
  }
  const context = {
    URL, TextDecoder, Uint8Array,
    window: { location: { hash: '#' + payload }, addEventListener: (event, fn) => { events[event] = fn; } },
    document: { getElementById: id => elements[id] },
    navigator: {},
  };
  vm.runInNewContext(fs.readFileSync(require.resolve('./invite.js'), 'utf8'), context);
  return { elements, context, events };
}

const rendered = render(pack('http://[::1]:8080'));
const page = rendered.elements;
assert.equal(page['invite-ready'].hidden, false);
assert.equal(page['server-host'].textContent, '[::1]:8080');
assert.equal(page['insecure-warning'].hidden, false);
assert.equal(page['open-tiny'].href, 'tinyclient://join#' + pack('http://[::1]:8080'));
assert.equal(page['invite-code'].value, pack('http://[::1]:8080'));
const invalidPage = render('broken').elements;
assert.equal(invalidPage['invite-error'].hidden, false);
assert.equal(invalidPage['invite-ready'].hidden, true);
assert.equal(invalidPage['open-tiny'].href, '');
rendered.context.window.location.hash = '#' + pack('https://second.example', 'second');
assert.equal(typeof rendered.events.hashchange, 'function');
rendered.events.hashchange();
assert.equal(page['server-host'].textContent, 'second.example');
assert.equal(page['insecure-warning'].hidden, true);
assert.equal(page['open-tiny'].href, 'tinyclient://join#' + pack('https://second.example', 'second'));
rendered.context.window.location.hash = '#broken';
rendered.events.hashchange();
assert.equal(page['invite-ready'].hidden, true);
assert.equal(page['invite-error'].hidden, false);
assert.equal(page['invite-code'].value, '');
assert.equal(page['server-host'].textContent, '');
assert.equal(page['open-tiny'].href, '');
console.log('invite parsing tests passed');
