/* Invite secrets stay in the fragment and are read only in this browser. */
(function () {
  "use strict";

  function parseInvite(payload) {
    if (payload.length > 2048) return null;
    var match = /^tc1\.([0-9a-fA-F]+)\.([A-Za-z0-9_-]{1,256})$/.exec(payload);
    if (!match || match[1].length % 2 || match[1].length > 1024) return null;
    var bytes = new Uint8Array(match[1].length / 2);
    for (var i = 0; i < bytes.length; i++) bytes[i] = parseInt(match[1].slice(i * 2, i * 2 + 2), 16);
    var origin;
    try { origin = new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch (_) { return null; }
    if (!origin || /[\s\u0000-\u001f\u007f\\]/.test(origin)) return null;
    var parts = origin.split('://');
    if (parts.length !== 2 || !parts[1] || /[\/?#@]/.test(parts[1]) || /[:.]$/.test(parts[1])) return null;
    var url;
    try { url = new URL(origin); } catch (_) { return null; }
    if ((url.protocol !== 'https:' && url.protocol !== 'http:') || !url.hostname || url.hostname.endsWith('.') || url.port === '0') return null;
    return { origin: url.origin, host: url.host, insecure: url.protocol === 'http:', payload: payload };
  }

  if (typeof module !== 'undefined' && module.exports) module.exports = { parseInvite: parseInvite };
  if (typeof document === 'undefined') return;
  var invite = null;
  var code = document.getElementById('invite-code');
  function renderInvite() {
    invite = parseInvite(window.location.hash.slice(1));
    document.getElementById('invite-error').hidden = !!invite;
    document.getElementById('invite-ready').hidden = !invite;
    document.getElementById('server-host').textContent = invite ? invite.host : '';
    document.getElementById('insecure-warning').hidden = !invite || !invite.insecure;
    document.getElementById('open-tiny').href = invite ? 'tinyclient://join#' + invite.payload : '';
    code.value = invite ? invite.payload : '';
    document.getElementById('copy-status').textContent = '';
  }
  window.addEventListener('hashchange', renderInvite);
  renderInvite();
  document.getElementById('copy-link').addEventListener('click', function () {
    if (!invite) return;
    var link = 'https://tinyclient.io/invite#' + invite.payload;
    if (!navigator.clipboard || !navigator.clipboard.writeText) {
      document.getElementById('copy-status').textContent = 'Could not copy. Copy the original link from your browser address bar.';
      return;
    }
    navigator.clipboard.writeText(link).then(function () {
      document.getElementById('copy-status').textContent = 'Link copied.';
    }, function () {
      document.getElementById('copy-status').textContent = 'Could not copy. Copy the original link from your browser address bar.';
    });
  });
}());
