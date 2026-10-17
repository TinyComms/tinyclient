// Serves a markdown rendering when the client asks for it, and sets the
// content types discovery documents need. HTML stays the default.

var LINK =
  '</.well-known/api-catalog>; rel="api-catalog", ' +
  '</docs/>; rel="service-doc", ' +
  '</.well-known/openapi.json>; rel="service-desc", ' +
  '</llms.txt>; rel="describedby", ' +
  '</.well-known/ai-catalog.json>; rel="describedby"';

var TYPES = {
  "/.well-known/api-catalog": "application/linkset+json; charset=utf-8",
  "/.well-known/openapi.json": "application/vnd.oai.openapi+json; charset=utf-8",
  "/.well-known/ai-catalog.json": "application/json; charset=utf-8",
  "/.well-known/agent-skills/index.json": "application/json; charset=utf-8",
  "/auth.md": "text/markdown; charset=utf-8",
  "/llms.txt": "text/plain; charset=utf-8",
  "/robots.txt": "text/plain; charset=utf-8",
  "/sitemap.xml": "application/xml; charset=utf-8",
};

// Pages only bundles a worker from site/_worker.js. A functions/ directory
// next to the assets is ignored by `wrangler pages deploy`, which looks for
// functions/ in the repo root.
export default {
  async fetch(request, env) {
    var response = await env.ASSETS.fetch(request);
    try {
      return await finish(request, response);
    } catch (err) {
      return response;
    }
  },
};

async function finish(request, response) {
  var url = new URL(request.url);
  var headers = new Headers(response.headers);
  var accept = request.headers.get("Accept") || "";
  var current = headers.get("Content-Type") || "";
  // Copy headers off the asset response (including the security headers from
  // _headers) and only build a new response when the body or media type changes.
  if (prefersMarkdown(accept) && current.indexOf("text/html") !== -1 && response.status === 200) {
    var markdown = htmlToMarkdown(await response.text());
    headers.set("Content-Type", "text/markdown; charset=utf-8");
    headers.set("x-markdown-tokens", String(tokenEstimate(markdown)));
    headers.delete("Content-Length");
    headers.delete("Content-Encoding");
    appendVary(headers, "Accept");
    if (url.pathname === "/" || url.pathname === "/index.html") headers.set("Link", LINK);
    return new Response(markdown, { status: 200, headers: headers });
  }
  var type = TYPES[url.pathname];
  if (type && current.toLowerCase().indexOf(type.split(";")[0]) === -1) {
    headers.set("Content-Type", type);
    return new Response(response.body, { status: response.status, headers: headers });
  }
  return response;
}

function prefersMarkdown(accept) {
  var parts = accept.split(",");
  var md = -1;
  var html = -1;
  for (var i = 0; i < parts.length; i++) {
    var piece = parts[i].trim();
    var q = 1;
    var match = /;\s*q=([0-9.]+)/i.exec(piece);
    if (match) q = parseFloat(match[1]);
    var media = piece.split(";")[0].trim().toLowerCase();
    if (media === "text/markdown") md = q;
    if (media === "text/html") html = q;
  }
  return md >= 0 && md >= html;
}

function tokenEstimate(text) {
  var n = Math.round(text.length / 4);
  return n > 0 ? n : 1;
}

function appendVary(headers, token) {
  var vary = headers.get("Vary");
  if (!vary) {
    headers.set("Vary", token);
    return;
  }
  if (vary.toLowerCase().indexOf(token.toLowerCase()) === -1) {
    headers.set("Vary", vary + ", " + token);
  }
}

export function htmlToMarkdown(html) {
  var title = attr(html, "title");
  var description = meta(html, "description");
  var main = /<main\b[^>]*>([\s\S]*?)<\/main>/i.exec(html);
  var body = main ? main[1] : html;
  body = body.replace(/<script\b[^>]*>[\s\S]*?<\/script>/gi, "");
  body = body.replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, "");
  body = body.replace(/<svg\b[^>]*>[\s\S]*?<\/svg>/gi, "");
  body = body.replace(/<noscript\b[^>]*>[\s\S]*?<\/noscript>/gi, "");
  body = body.replace(/<h1\b[^>]*>([\s\S]*?)<\/h1>/gi, function (_, t) { return "\n# " + inline(t) + "\n\n"; });
  body = body.replace(/<h2\b[^>]*>([\s\S]*?)<\/h2>/gi, function (_, t) { return "\n## " + inline(t) + "\n\n"; });
  body = body.replace(/<h3\b[^>]*>([\s\S]*?)<\/h3>/gi, function (_, t) { return "\n### " + inline(t) + "\n\n"; });
  body = body.replace(/<li\b[^>]*>([\s\S]*?)<\/li>/gi, function (_, t) { return "\n- " + inline(t); });
  body = body.replace(/<pre\b[^>]*>([\s\S]*?)<\/pre>/gi, function (_, t) {
    return "\n\n```\n" + decode(t.replace(/<[^>]+>/g, "")) + "\n```\n\n";
  });
  body = body.replace(/<p\b[^>]*>([\s\S]*?)<\/p>/gi, function (_, t) { return "\n" + inline(t) + "\n"; });
  body = body.replace(/<figcaption\b[^>]*>([\s\S]*?)<\/figcaption>/gi, function (_, t) { return "\n*" + inline(t) + "*\n"; });
  body = body.replace(/<br\s*\/?>/gi, "\n");
  body = strip(body);
  body = decode(body).replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
  var head = [];
  if (title) head.push("# " + title);
  if (description) head.push(description);
  if (body.indexOf("# ") === 0) head = description ? [description] : [];
  return (head.concat(body ? [body] : []).join("\n\n")).trim() + "\n";
}

function attr(html, tag) {
  var match = new RegExp("<" + tag + "\\b[^>]*>([\\s\\S]*?)</" + tag + ">", "i").exec(html);
  return match ? decode(match[1].replace(/<[^>]+>/g, "")).trim() : "";
}

function meta(html, name) {
  var match = new RegExp("<meta\\b[^>]*name=[\"']" + name + "[\"'][^>]*>", "i").exec(html);
  if (!match) return "";
  var content = /content=["']([^"']*)["']/i.exec(match[0]);
  return content ? decode(content[1]).trim() : "";
}

function inline(html) {
  var text = html.replace(/<a\b[^>]*href=["']([^"']+)["'][^>]*>([\s\S]*?)<\/a>/gi, function (_, href, label) {
    return "[" + strip(label) + "](" + decode(href) + ")";
  });
  text = text.replace(/<code\b[^>]*>([\s\S]*?)<\/code>/gi, function (_, c) { return "`" + strip(c) + "`"; });
  text = text.replace(/<(strong|b)\b[^>]*>([\s\S]*?)<\/\1>/gi, function (_, _t, c) { return "**" + strip(c) + "**"; });
  return strip(text).replace(/\s+/g, " ").trim();
}

function strip(html) {
  return html.replace(/<[^>]+>/g, "");
}

function decode(text) {
  return text
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;|&apos;/g, "'")
    .replace(/&rsquo;|&lsquo;/g, "'")
    .replace(/&rdquo;|&ldquo;/g, '"')
    .replace(/&mdash;/g, "—")
    .replace(/&ndash;/g, "–")
    .replace(/&#(\d+);/g, function (_, n) { return String.fromCharCode(parseInt(n, 10)); });
}
