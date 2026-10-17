// The working model of the tinyclient window in the hero (index.html). It takes the still
// screenshot's place at 960 px and wider, and is the app's own layout, colours and built-in demo
// (crates/chatd/src/demo/) drawn in HTML: the same people, servers, channels and messages, at the
// sizes in crates/tinyclient/src/gui/. Nothing is sent anywhere. What a visitor types only ever
// becomes text nodes in this page.
(function () {
  "use strict";
  var stage = document.getElementById("tc-stage");
  if (!stage || !window.matchMedia) return;
  var wide = window.matchMedia("(min-width: 960px)");

  // Lucide glyphs, as crates/tinyclient/assets/icons has them.
  var ICON = {
    "inbox":'<polyline points="22 12 16 12 14 15 10 15 8 12 2 12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>',
    "users":'<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>',
    "plus":'<path d="M5 12h14"/><path d="M12 5v14"/>',
    "settings":'<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>',
    "list":'<path d="M3 12h.01"/><path d="M3 18h.01"/><path d="M3 6h.01"/><path d="M8 12h13"/><path d="M8 18h13"/><path d="M8 6h13"/>',
    "arrow-down":'<path d="M12 5v14"/><path d="m19 12-7 7-7-7"/>',
    "search":'<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
    "square-pen":'<path d="M12 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/><path d="M18.375 2.625a1 1 0 0 1 3 3l-9.013 9.014a2 2 0 0 1-.853.505l-2.873.84a.5.5 0 0 1-.62-.62l.84-2.873a2 2 0 0 1 .506-.852z"/>',
    "chevron-left":'<path d="m15 18-6-6 6-6"/>',
    "chevron-right":'<path d="m9 18 6-6-6-6"/>',
    "chevron-down":'<path d="m6 9 6 6 6-6"/>',
    "hash":'<line x1="4" x2="20" y1="9" y2="9"/><line x1="4" x2="20" y1="15" y2="15"/><line x1="10" x2="8" y1="3" y2="21"/><line x1="16" x2="14" y1="3" y2="21"/>',
    "info":'<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
    "paperclip":'<path d="M13.234 20.252 21 12.3"/><path d="m16 6-8.414 8.586a2 2 0 0 0 0 2.828 2 2 0 0 0 2.828 0l8.414-8.586a4 4 0 0 0 0-5.656 4 4 0 0 0-5.656 0l-8.415 8.585a6 6 0 1 0 8.486 8.486"/>',
    "smile":'<circle cx="12" cy="12" r="10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" x2="9.01" y1="9" y2="9"/><line x1="15" x2="15.01" y1="9" y2="9"/>',
    "send-horizontal":'<path d="M3.714 3.048a.498.498 0 0 0-.683.627l2.843 7.627a2 2 0 0 1 0 1.396l-2.842 7.627a.498.498 0 0 0 .682.627l18-8.5a.5.5 0 0 0 0-.904z"/><path d="M6 12h16"/>',
    "volume-2":'<path d="M11 4.702a.705.705 0 0 0-1.203-.498L6.413 7.587A1.4 1.4 0 0 1 5.416 8H3a1 1 0 0 0-1 1v6a1 1 0 0 0 1 1h2.416a1.4 1.4 0 0 1 .997.413l3.383 3.384A.705.705 0 0 0 11 19.298z"/><path d="M16 9a5 5 0 0 1 0 6"/><path d="M19.364 18.364a9 9 0 0 0 0-12.728"/>',
    "megaphone":'<path d="m3 11 18-5v12L3 14v-3z"/><path d="M11.6 16.8a3 3 0 1 1-5.8-1.6"/>',
    "mic":'<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" x2="12" y1="19" y2="22"/>',
    "mic-off":'<line x1="2" x2="22" y1="2" y2="22"/><path d="M18.89 13.23A7.12 7.12 0 0 0 19 12v-2"/><path d="M5 10v2a7 7 0 0 0 12 5"/><path d="M15 9.34V5a3 3 0 0 0-5.68-1.33"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12"/><line x1="12" x2="12" y1="19" y2="22"/>',
    "headphones":'<path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3"/>',
    "headphone-off":'<path d="M21 14h-1.343"/><path d="M9.128 3.47A9 9 0 0 1 21 12v3.343"/><path d="m2 2 20 20"/><path d="M20.414 20.414A2 2 0 0 1 19 21h-1a2 2 0 0 1-2-2v-3"/><path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 2.636-6.364"/>',
    "video":'<path d="m16 13 5.223 3.482a.5.5 0 0 0 .777-.416V7.87a.5.5 0 0 0-.752-.432L16 10.5"/><rect x="2" y="6" width="14" height="12" rx="2"/>',
    "monitor-up":'<path d="m9 10 3-3 3 3"/><path d="M12 13V7"/><rect width="20" height="14" x="2" y="3" rx="2"/><path d="M12 17v4"/><path d="M8 21h8"/>',
    "phone-off":'<path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7 2 2 0 0 1 1.72 2v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91"/><line x1="22" x2="2" y1="2" y2="22"/>',
    "phone":'<path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z"/>',
    "x":'<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
    "smile-plus":'<path d="M22 11v1a10 10 0 1 1-9-10"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" x2="9.01" y1="9" y2="9"/><line x1="15" x2="15.01" y1="9" y2="9"/><path d="M16 5h6"/><path d="M19 2v6"/>',
    "reply":'<polyline points="9 17 4 12 9 7"/><path d="M20 18v-2a4 4 0 0 0-4-4H4"/>',
    "lock":'<rect width="18" height="11" x="3" y="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
    "messages-square":'<path d="M14 9a2 2 0 0 1-2 2H6l-4 4V4a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2z"/><path d="M18 9h2a2 2 0 0 1 2 2v11l-4-4h-6a2 2 0 0 1-2-2v-1"/>',
    "badge-check":'<path d="M3.85 8.62a4 4 0 0 1 4.78-4.77 4 4 0 0 1 6.74 0 4 4 0 0 1 4.78 4.78 4 4 0 0 1 0 6.74 4 4 0 0 1-4.77 4.78 4 4 0 0 1-6.75 0 4 4 0 0 1-4.78-4.77 4 4 0 0 1 0-6.76Z"/><path d="m9 12 2 2 4-4"/>',
    "bookmark":'<path d="m19 21-7-4-7 4V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2v16z"/>',
    "ellipsis":'<circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/>',
    "user":'<path d="M19 21v-2a4 4 0 0 0-4-4H9a4 4 0 0 0-4 4v2"/><circle cx="12" cy="7" r="4"/>',
    "arrow-up-right":'<path d="M7 7h10v10"/><path d="M7 17 17 7"/>',
    "check":'<path d="M20 6 9 17l-5-5"/>',
    "pin":'<path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z"/>',
    "corner-up-left":'<polyline points="9 14 4 9 9 4"/><path d="M20 20v-7a4 4 0 0 0-4-4H4"/>',
    "message-square":'<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>',
    "check-check":'<path d="M18 6 7 17l-5-5"/><path d="m22 10-7.5 7.5L13 16"/>',
    "corner-down-right":'<polyline points="15 10 20 15 15 20"/><path d="M4 4v7a4 4 0 0 0 4 4h12"/>',
    "mail":'<rect width="20" height="16" x="2" y="4" rx="2"/><path d="m22 7-8.97 5.7a1.94 1.94 0 0 1-2.06 0L2 7"/>',
    "calendar":'<path d="M8 2v4"/><path d="M16 2v4"/><rect width="18" height="18" x="3" y="4" rx="2"/><path d="M3 10h18"/>'
  };
  function ic(name, size) {
    var s = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    s.setAttribute("viewBox", "0 0 24 24");
    s.setAttribute("width", size || 16);
    s.setAttribute("height", size || 16);
    s.setAttribute("aria-hidden", "true");
    s.innerHTML = ICON[name];
    return s;
  }
  // h("button.row.sel", {attrs, on: {click: f}}, kids...): classes take the tc- prefix.
  function h(spec, attrs) {
    var parts = spec.split("."), el = document.createElement(parts[0] || "div");
    if (parts.length > 1) el.className = parts.slice(1).map(function (c) { return "tc-" + c; }).join(" ");
    for (var k in attrs || {}) {
      var v = attrs[k];
      if (k === "on") for (var e in v) el.addEventListener(e, v[e]);
      else if (v != null && v !== false) el.setAttribute(k, v === true ? "" : v);
    }
    for (var i = 2; i < arguments.length; i++) put(el, arguments[i]);
    return el;
  }
  function put(el) {
    for (var i = 1; i < arguments.length; i++) {
      var kid = arguments[i];
      if (kid == null || kid === false) continue;
      if (Array.isArray(kid)) kid.forEach(function (k) { put(el, k); });
      else el.appendChild(typeof kid === "object" ? kid : document.createTextNode(String(kid)));
    }
  }
  function net(n, size) { return h("img", { src: "assets/networks/" + (n === "teams" ? "microsoft-teams" : n) + ".svg", alt: "", width: size, height: size }); }
  var NAMES = { discord: "Discord", slack: "Slack", telegram: "Telegram", teams: "Teams", signal: "Signal", messages: "Messages" };
  var NETS = ["discord", "slack", "telegram", "teams", "signal", "messages"];

  // Time: the demo's "now" is 2:54 PM on Saturday 17 October; every message is minutes before it.
  var NOW = 14 * 60 + 54, DAY = 1440;
  var WD = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
  var MO = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
  function dayOf(ago) { return ago <= NOW ? 0 : Math.ceil((ago - NOW) / DAY); }
  function clock(ago) {
    var m = ((NOW - ago) % DAY + DAY) % DAY, hr = Math.floor(m / 60), mi = m % 60;
    return (hr % 12 || 12) + ":" + (mi < 10 ? "0" : "") + mi + (hr < 12 ? " AM" : " PM");
  }
  function on(d) { return new Date(2026, 9, 17 - d); }
  function dayLine(ago) {
    var d = dayOf(ago), x = on(d);
    return d === 0 ? "Today" : d === 1 ? "Yesterday" : WD[x.getDay()] + ", " + MO[x.getMonth()] + " " + x.getDate();
  }
  function rowTime(ago) {
    var d = dayOf(ago), x = on(d);
    return d === 0 ? clock(ago) : d === 1 ? "Yesterday" : d < 7 ? WD[x.getDay()].slice(0, 3) : MO[x.getMonth()].slice(0, 3) + " " + x.getDate();
  }

  // People: name, picture in assets/demo, presence (o online, i idle, d do not disturb,
  // b busy, f offline), status, and 1 for an app.
  var P = {
    me: ["Robin Vale", "robin-vale", "o"],
    ines: ["Ines Marchetti", "ines-marchetti", "o", "sketching trail maps"], tomasz: ["Tomasz Brandt", "tomasz-brandt", "o"],
    wren: ["Wren Halloran", "wren-halloran", "i", "back in 20"], odalys: ["Odalys Ferreira", "odalys-ferreira", "d", "deep work until 3"],
    kofi: ["Kofi Asante", "kofi-asante", "f"], mirela: ["Mirela Dragan", "mirela-dragan", "o", "🎧 synthwave"],
    haruto: ["Haruto Senda", "haruto-senda", "i"], dita: ["Dita Novak", "dita-novak", "o"],
    yusuf: ["Yusuf Demirci", "yusuf-demirci", "o", "shipping the thing"], rafael: ["Rafael Quintero", "rafael-quintero", "d", "in a meeting"],
    bot: ["Lichen Bot", "lichen-bot", "", "", 1],
    mara: ["Mara Okafor", "mara-okafor", "o", "Reviewing the migration plan"], dev: ["Dev Patel", "dev-patel", "o"],
    priya: ["Priya Raman", "priya-raman", "d", "In planning until 2"], devon: ["Devon Reyes", "devon-reyes", "f", "On leave until the 21st"],
    anneke: ["Anneke Visser", "", "o"], tunde: ["Tunde Bakare", "", "f"], sofia: ["Sofia Lindgren", "", "o", "🌴 Working from Lisbon"],
    deploy: ["Deploy Bot", "", "", "", 1], imogen: ["Imogen Castellanos", "imogen-castellanos", "o"], noor: ["Noor Haddad", "", "o"],
    lena: ["Lena Brandt", "lena-brandt", "o"], rosa: ["Grandma Rosa", "grandma-rosa"], weather: ["Weather Bot", "weather-bot", "", "", 1],
    scam: ["Crypto Giveaway Official", "crypto-giveaway-official"], tg: ["Telegram", "telegram"], daily: ["Lichen Daily", ""],
    ana: ["Ana Ruiz", "ana-ruiz", "b", "In a meeting"], ben: ["Ben Okoro", "ben-okoro", "o"], dmitri: ["Dmitri Volkov", "", "i"],
    esi: ["Esi Mensah", "", "d"], flows: ["Workflows", "workflows", "", "", 1],
    theo: ["Theo Marchetti", "theo-marchetti"], sam: ["Sam Achterberg", "sam-achterberg"], leila: ["Leila Haddad", "leila-haddad"],
    elio: ["Nonno Elio", "nonno-elio"], chloe: ["Chloe Martin", "chloe-martin"], pharmacy: ["Riverside Pharmacy", ""]
  };
  var PRESENT = { discord: 1, slack: 1, telegram: 1, teams: 1 };
  // Spaces: name, icon.
  var SP = {
    moss: ["Moss & Lichen", "icon-moss-lichen"], rust: ["Rustaceans Anonymous", "icon-rustaceans"], synth: ["Synth DIY", "icon-synth-diy"],
    pixel: ["Pixel Foundry", "logo-pixel-foundry"], night: ["Night Shift Games", "icon-night-shift"], tiny: ["Tiny Tools", "logo-tiny-tools"],
    society: ["The Society for the Preservation of Unreasonably Long Server Names and Their Abbreviations", "logo-long-names"],
    volcano: ["🌋 Volcano Club 🔥", "logo-volcano-club"], harbor: ["Harbor Labs", "logo-harbor-labs"], fern: ["Fernhill Collective", "icon-fernhill"],
    eng: ["Harbor Engineering", "logo-harbor-engineering"], social: ["Harbor Social", "logo-harbor-social"]
  };

  // Conversations. A message is [who, minutes ago, text, extras]: r reactions [emoji, count,
  // mine], ed edited, th a thread, rp [who, the words quoted] (no words: deleted), pin, sys, card.
  var C = {};
  function cv(id, n, kind, title, o) {
    o = o || {};
    o.id = id; o.n = Object.keys(C).length; o.net = n; o.kind = kind; o.title = title;
    o.u = o.u || 0; o.mt = o.mt || 0; o.nw = o.nw || 0;
    o.ms = (o.ms || []).map(msg);
    C[id] = o;
    return o;
  }
  var seq = 0;
  function msg(a) { var x = a[3] || {}; x.w = a[0]; x.a = a[1]; x.t = a[2]; x.id = "m" + seq++; return x; }

  // Servers: a category, then "name:flags": a announcement, v voice, f forum, t thread, m muted,
  // p private, uN unread, @N mentions.
  var TREES = {
    moss: [["", "rules:a,announcements:au2"], ["Start here", "welcome,introductions,roles:m"],
      ["Trail", "general:u5@2,showcase:u4@1,Trail camera build log:tu2,trail-cams:u5,field-notes,route-planning:mu9,gear-swap:mu6@1,photo-dump"],
      ["Identification", "mosses,lichens:u1,fungi,ferns,mystery-specimens:u3,id-help:f"],
      ["Projects", "herbarium-scans,microscope-club,map-layers:u1@1,citizen-science,grant-writing:m"],
      ["Meetups", "spring-foray:u4,autumn-foray,carpools,lodging"], ["Off trail", "off-topic,cooking,music:m,pets:u1,books,games"],
      ["Voice", "Campfire:v,Field recording:v,Quiet room:v"]],
    rust: [["Talk", "general,beginners,show-and-tell"]], synth: [["Bench", "general:u2,build-logs,troubleshooting"], ["Market", "wanted,for-sale"]],
    pixel: [["Studio", "general:u2@1,work-in-progress,critique,palettes"]], night: [["Lobby", "general,looking-for-group"], ["Games", "co-op,strategy,clips"]],
    tiny: [["Workshop", "general,releases,feedback"]], society: [["Proceedings", "general,minutes-of-the-last-meeting,points-of-order"]],
    volcano: [["Crater", "general,eruptions,🌡️-readings"]]
  };
  var TOPIC = { general: "Trail talk, plans and frost reports. Photos go in photo-dump.", showcase: "One of everything a message can hold.",
    "trail-cams": "Camera status and what they caught overnight", rules: "Read these first", "id-help": "One post per specimen, with a scale in the photo" };
  var KIND = { a: "ann", v: "voice", f: "forum", t: "thread" };
  Object.keys(TREES).forEach(function (sp) {
    TREES[sp] = TREES[sp].map(function (cat) {
      return [cat[0], cat[1].split(",").map(function (spec) {
        var p = spec.split(":"), f = p[1] || "", id = sp + "/" + p[0];
        cv(id, "discord", KIND[f.replace(/[^avft]/g, "")] || "ch", p[0], {
          sp: sp, g: cat[0], topic: sp === "moss" ? TOPIC[p[0]] : "", muted: /m/.test(f),
          u: +(f.match(/u(\d+)/) || [0, 0])[1], mt: +(f.match(/@(\d+)/) || [0, 0])[1], call: f === "v" ? "voice" : ""
        });
        return id;
      })];
    });
  });
  function script(id, ms, more) { var c = C[id]; c.ms = ms.map(msg); for (var k in more || {}) c[k] = more[k]; }

  script("moss/general", [["ines", 3200, "Morning all. Frost on the north slope, so the cushion mosses are finally showing their capsules."], ["tomasz", 3190, "Bringing the macro lens on Saturday then."],
    ["wren", 3110, "Does anyone have a spare hand lens? Mine went into the river last week."], ["haruto", 3105, "I have two. One is yours if you carry the tripod."], ["wren", 3104, "Deal."],
    ["dita", 1620, "The county finally published the new trail closures. Ridge loop is shut until April."], ["ines", 1611, "That is the one with the old-growth stand. Shame."],
    ["me", 1600, "We could do the creek route instead. Wetter, but more liverworts than anywhere else nearby."], ["tomasz", 1598, "Creek route has my vote."],
    ["kofi", 1590, "Mine too, as long as someone brings a thermos."], ["mirela", 420, "Scanned forty herbarium sheets last night. The 1911 ones are in better shape than the 1970s ones."],
    ["odalys", 415, "Better paper, worse handwriting."], ["mirela", 414, "So much worse handwriting."], ["haruto", 190, "Reminder that the microscope club meets Thursday. Bring slides, not opinions."],
    ["ines", 62, "@Robin Vale are you still bringing the trail camera on Saturday?"], ["tomasz", 55, "Also the spare batteries, the last set died by noon."],
    ["dita", 31, "Carpool list is pinned in the meetups channel."], ["wren", 12, "Found something odd on a fence post. Photos in mystery-specimens."],
    ["ines", 4, "@Robin Vale need a yes or no on the camera before I book the van."]], { nw: 5, inbox: 1 });
  var R = function (s) { return s.split(" ").map(function (x) { return [x.replace(/\d+\*?$/, ""), +x.match(/\d+/)[0], /\*$/.test(x)]; }); };
  script("moss/showcase", [["tomasz", 274, "County recorder confirmed it: first pine marten on this ridge since 1987.", { r: R("👍4* ❤️12 🎉9* 👀7 🔥5 💯3 😂3 🙏2 🚀2 🤔1 🌿1") }],
    ["me", 268, "Same spot as this one. The camera was pointing the right way for once.", { rp: ["ines", "Frost on the cushion moss this morning."] }],
    ["haruto", 266, "Seconded, whatever it said.", { rp: ["wren", ""] }],
    ["tomasz", 254, "Trail camera build log. Notes go in the thread so the channel stays readable.", { th: "build" }],
    ["haruto", 200, "Microscope club is Thursday at 19:00, not 18:00.", { ed: 1 }],
    ["ines", 140, "Saturday: usual corner, 7:30, creek route. Boots, hand lens, packed lunch.", { pin: 1 }],
    ["ines", 139, "Ines Marchetti pinned a message to this channel.", { sys: 1 }], ["tomasz", 99, "Tomasz Brandt started a call that lasted 14 minutes.", { sys: 1 }],
    ["wren", 69, "You missed a call from Wren Halloran.", { sys: 1 }],
    ["bot", 39, "Nightly camera report", { card: ["3 cameras online", "41 frames kept overnight. One card is nearly full and one battery will not last the week.",
      ["Camera 1", "Card 94% full", "Camera 2", "Pine marten, 03:12", "Camera 3", "Battery 11%", "Next report", "Tomorrow, 06:00"], "Lichen Bot · trail-cams"] }],
    ["ines", 12, "@Robin Vale can you check #trail-cams with @Field Team before Saturday? @Tomasz Brandt has the keys. Heads up @everyone: the creek route is on."],
    ["tomasz", 2, "I do have the keys. I checked twice."]], { nw: 7, inbox: 1, pinned: 1 });
  C["moss/Trail camera build log"].opens = ["moss/showcase", "build"];
  script("moss/trail-cams", [["tomasz", 900, "Camera 2 caught a pine marten at 03:12. First one on this ridge in years."], ["wren", 880, "That tail! Send the clip to the county recorder."],
    ["tomasz", 300, "Battery on camera 3 is at 11%. Who is up there next?"], ["haruto", 290, "I am, Thursday. I will swap it."],
    ["tomasz", 45, "Camera 1 has gone quiet. Either the card is full or a deer ate it again."]], { nw: 5 });
  script("moss/mystery-specimens", [["wren", 14, "Fence post, north side, about a metre up. Grey-green, powdery edges."], ["odalys", 11, "Lepraria, probably. Do you have a closer shot?"],
    ["wren", 9, "Phone would not focus. Going back tomorrow with the hand lens."]], { nw: 3 });
  script("moss/announcements", [["ines", 9000, "Welcome to the new members from the county naturalists' list. Read the rules, then say hello in introductions."],
    ["ines", 2900, "Spring foray is set for the second weekend of April. Sign-up sheet goes out Monday."], ["ines", 130, "Sign-up sheet is live. Twenty places, lodging for twelve."]], { nw: 2 });
  script("moss/gear-swap", [["kofi", 700, "Selling a barely used field press. Too big for my flat."], ["dita", 640, "Robin was after one of those, I think?"]]);
  script("moss/map-layers", [["haruto", 150, "@Robin Vale the closures layer is ready for you to check against the county map."]], { nw: 1, inbox: 1 });
  script("moss/route-planning", [["dita", 500, "Creek route is 11 km with the detour. Four hours at foray pace."], ["haruto", 480, "Foray pace being one kilometre an hour and a lot of kneeling."]]);
  script("pixel/general", [["haruto", 1200, "Quiet week. Anyone building anything?"], ["rafael", 1180, "Slowly. Photos when it stops looking like a fire hazard."]], { nw: 2, inbox: 1 });
  script("synth/general", [["odalys", 600, "Quiet week. Anyone building anything?"], ["mirela", 590, "Slowly. Photos when it stops looking like a fire hazard."]], { nw: 2 });
  C["moss/Campfire"].seats = ["ines", "tomasz", "wren"];
  C["moss/Field recording"].seats = ["yusuf"];
  C["moss/Quiet room"].seats = ["kofi", "mirela", "haruto"]; C["moss/Quiet room"].more = 28;

  var T = { build: { ms: [["tomasz", 260, "Day 1: the sensor triggers on wind. Moved it behind the lens hood."], ["haruto", 250, "Try a longer debounce before you move anything else."],
    ["tomasz", 200, "Day 2: 400 ms debounce, no more wind, still catches the marten."], ["me", 190, "Battery draw?"], ["tomasz", 185, "11 mA idle. A week on one pack."],
    ["ines", 75, "Put the diagram in the showcase when it is done."]].map(msg), nw: 2, faces: ["ines", "tomasz", "haruto"], u: 2 } };

  var DM = function (id, n, who, o) { o = o || {}; o.peer = who; return cv(id, n, "dm", P[who][0], o); };
  DM("d-ines", "discord", "ines", { u: 3, nw: 3, start: 1, call: "dm", ms: [["ines", 2890, "Did you see the frost flowers by the weir this morning?"], ["me", 2885, "Missed them. I was at the wrong weir."],
    ["ines", 2884, "There is only one weir."], ["me", 2880, "Then I was at the wrong river."], ["ines", 9, "Van is booked for Saturday, 7:30 at the usual corner."],
    ["ines", 8, "Can you bring the trail camera and the long tripod?"], ["ines", 7, "And tell Tomasz, he never reads the channel."]] });
  cv("d-carpool", "discord", "grp", "Foray carpool", { av: "foray-carpool", u: 12, nw: 12, call: "dm", ms: [["dita", 400, "Who is driving on Saturday?"], ["me", 395, "Ines has the van. I can take three more."],
    ["haruto", 60, "I need a seat and space for a tripod."], ["dita", 58, "Same, minus the tripod."], ["ines", 50, "Van has seven seats. Robin, you take the gear."], ["haruto", 44, "Leaving from the usual corner?"],
    ["ines", 43, "Usual corner, 7:30."], ["dita", 40, "I will bring pastries."], ["haruto", 39, "This is why we invite Dita."], ["ines", 30, "Weather says dry until two."], ["dita", 22, "Boots anyway."],
    ["haruto", 21, "Always boots."], ["ines", 16, "Robin, confirm you have the camera?"], ["dita", 15, "And the thermos."]] });
  DM("d-tomasz", "discord", "tomasz", { start: 1, call: "dm", ms: [["tomasz", 1500, "Do you still have my soldering iron?"], ["me", 1490, "I do. It is on my desk, judging me."],
    ["tomasz", 1488, "Bring it Saturday and all is forgiven."], ["me", 70, "Ines says 7:30 at the usual corner. Bring batteries."]] });
  DM("d-mirela", "discord", "mirela", { muted: 1, u: 1, mt: 1, nw: 1, start: 1, call: "dm", ms: [["mirela", 3100, "New mix is up if you want something for the drive."], ["me", 3000, "Listening now. Track three is doing a lot."],
    ["mirela", 95, "@Robin Vale you left your gloves in my car."]] });
  DM("d-yusuf", "discord", "yusuf", { start: 1, call: "dm", ms: [["yusuf", 210, "Firmware for the trail camera is nearly there. Motion trigger works, timestamps are wrong by an hour."], ["me", 205, "Daylight saving?"], ["yusuf", 204, "Daylight saving."]] });
  DM("d-odalys", "discord", "odalys", { muted: 1, u: 2, nw: 2, start: 1, call: "dm", ms: [["odalys", 5000, "Sending you the draft key for the Cladonia section."], ["me", 4990, "Got it. I will read it on the train."],
    ["odalys", 260, "Page 4 has a mistake in couplet 12, ignore it."], ["odalys", 255, "And page 9. Ignore page 9 entirely."]] });
  DM("d-kofi", "discord", "kofi", { start: 1, call: "dm", ms: [["kofi", 12000, "Thanks for the lift back. I owe you a coffee."], ["me", 11990, "You owe me three. I am keeping count."]] });

  var SL = function (name, f, ms, more) { var o = more || {}; o.sp = "harbor"; o.g = f; o.ms = ms; o.start = 1; return cv("h/" + name, "slack", "ch", name, o); };
  SL("eng-platform", "Engineering", [["mara", 2000, "@channel the pager test at 16:00 is real. Acknowledge within five minutes. Details in #eng-incidents."],
    ["mara", 1510, "Heads up: the migration for the events table is queued for tomorrow morning."], ["me", 1502, "Sounds good. I will keep the consumers paused until it finishes."],
    ["mara", 1450, "Plan for the events migration is in the doc. Questions in this thread please.", { th: "plan" }], ["mara", 41, "Migration finished clean. Resuming consumers now."],
    ["mara", 40, "Lag is back under a second."], ["dev", 12, "Canary looks healthy on my side, no new errors in the last 20 minutes."], ["mara", 3, "Deploy is green, rolling to prod at 3."]],
    { u: 4, nw: 5, topic: "Queues, consumers and the events pipeline. Runbook: go/platform" });
  SL("eng-incidents", "Engineering", [["tunde", 300, "Paging: checkout latency p99 is at 4 s in eu-west."], ["dev", 296, "Looking. It lines up with the cache node swap."],
    ["tunde", 280, "@Robin Vale did the consumer pause go out to eu-west as well?"], ["dev", 270, "Rolled the node back. p99 is recovering."],
    ["priya", 262, "Customers saw about 9 minutes of slow checkouts. Drafting the note."], ["tunde", 250, "@Robin Vale can you own the write-up? You have the timeline."],
    ["dev", 240, "Marking resolved. Follow-ups in the thread."]], { u: 6, mt: 2, nw: 6, inbox: 1, topic: "One incident, one thread. Page first, post second." });
  SL("announcements", "Company", [["anneke", 6000, "Quarterly planning starts Monday. Bring one thing you want to stop doing."], ["anneke", 180, "All-hands moved to Thursday 15:00. Recording will be posted here."]], { u: 1, nw: 1 });
  SL("general", "Company", [["anneke", 2800, "Office is closed Friday for the floor refit. Work from wherever has coffee."], ["dev", 2790, "So, the usual."], ["sofia", 400, "Lisbon office says hello. It is 24 degrees and I am not sorry."]]);
  SL("random", "Company", [["dev", 60, "Whoever left the oat milk out: it is a science project now."]], { muted: 1, u: 7 });
  SL("eng-deploys", "Engineering", [["deploy", 190, "api 4f2c1a9 deployed to staging by dev"], ["deploy", 120, "api 4f2c1a9 deployed to production by mara"], ["deploy", 30, "web 91be0d3 deployed to staging by anneke"]], { muted: 1, u: 20 });
  SL("design-crit", "Product", [["anneke", 700, "New empty states are in the file. Be unkind."], ["sofia", 90, "The illustration is carrying the whole screen. What does it say without it?"], ["anneke", 85, "Less. Which is the problem, yes."]], { u: 2, nw: 2 });
  SL("customer-voice", "Product", [["priya", 900, "Three accounts asked for audit log export this week."], ["priya", 500, "@Robin Vale is export on the platform roadmap or ours?"], ["devon", 480, "Ours, last I heard."]],
    { muted: 1, priv: 1, u: 3, mt: 1, nw: 2, topic: "What customers asked for this week" });
  cv("f/volunteers", "slack", "ch", "volunteers", { sp: "fern", g: "Channels", u: 2, mt: 1, nw: 1, inbox: 1, start: 1, topic: "Rotas and who has the keys",
    ms: [["noor", 500, "We need two more people for the Saturday watering rota."], ["imogen", 110, "@Robin Vale could you take the 9:00 slot this week?"]] });
  T.plan = { ms: [["dev", 1400, "Do we need the consumers paused for the whole run?"], ["mara", 1395, "Only for the backfill step, about ten minutes."],
    ["dev", 1390, "Then I will pause them from the runbook, not by hand."]].map(msg), nw: 1, faces: ["dev", "mara"], u: 1 };
  DM("s-mara", "slack", "mara", { sp: "harbor", u: 2, nw: 2, start: 1, ms: [["mara", 130, "Can you take the on-call handoff today?"], ["me", 124, "On it."],
    ["mara", 20, "Thanks. Runbook link is in the channel topic."], ["mara", 19, "Also: the pager test at 4 is real this time."]] });
  cv("s-maradev", "slack", "grp", "Mara, Dev", { sp: "harbor", duo: ["mara", "dev"], u: 3, nw: 3, start: 1, ms: [["mara", 500, "Who is writing the incident note?"], ["dev", 96, "Robin has the timeline, I have the graphs."],
    ["mara", 88, "Then Robin writes, Dev pastes, I edit."], ["dev", 80, "Democracy."]] });
  DM("s-priya", "slack", "priya", { sp: "harbor", muted: 1, u: 3, nw: 3, start: 1, mark: "📅", ms: [["priya", 700, "Planning doc is ready for comments."], ["priya", 650, "No rush."], ["priya", 640, "Some rush."]] });
  DM("s-dev", "slack", "dev", { sp: "harbor", start: 1, ms: [["dev", 1300, "Review is up for the consumer pause change."], ["me", 1290, "Approved with one nit about the timeout."], ["dev", 1285, "Fair nit. Fixed."]] });
  DM("s-devon", "slack", "devon", { sp: "harbor", start: 1, ms: [["devon", 9000, "Thanks for the review, merged. See you after the 21st."]] });

  var TG = function (id, kind, title, o) { return cv(id, "telegram", kind, title, o); };
  TG("t-saved", "dm", "Saved Messages", { glyph: "bookmark", start: 1, ms: [["me", 4000, "Locker code 4417"], ["me", 30, "Buy: film, batteries, chalk"]] });
  DM("t-ines", "telegram", "ines", { u: 3, nw: 2, start: 1, ms: [["me", 600, "Did the herbarium send the scans?"], ["ines", 40, "They did. Uploading them tonight."], ["ines", 38, "Also: are you in for Sunday?"]] });
  DM("t-tg", "telegram", "tg", { u: 1, nw: 1, verified: 1, ro: 1, ms: [["tg", 2, "Login code: 51923. Do not give this code to anyone, even if they say they are from Telegram!"]] });
  TG("t-myco", "forum", "Mycology Forum", { av: "mycology", ms: [["lena", 5, "ID requests: new photos of a bolete"]] });
  TG("t-forage", "grp", "Foraging Lisbon", { av: "foraging-lisbon", u: 3, mt: 1, nw: 3, start: 1, ms: [["ines", 500, "Chanterelles at Sintra, north side of the ridge.", { r: R("🔥3 ❤️2*") }],
    ["lena", 120, "Going Saturday. Room for two in the car."], ["mara", 18, "ok"], ["ines", 10, "@Robin Vale are you bringing the basket?"]] });
  DM("t-scam", "telegram", "scam", { tag: "Scam", start: 1, ms: [["scam", 15, "Congratulations! You won 2 BTC. Send 0.1 to claim."]] });
  DM("t-weather", "telegram", "weather", { app: 1, tag: "bot", u: 1, nw: 1, start: 1, ms: [["weather", 20, "Lisbon: 21°, sun, wind 12 km/h from the north."]] });
  DM("t-lena", "telegram", "lena", { start: 1, ms: [["lena", 900, "Which train are you on?"], ["me", 25, "The 18:40, carriage 4."]] });
  DM("t-rosa", "telegram", "rosa", { u: 1, nw: 1, start: 1, ms: [["rosa", 70, "Sunday lunch at one. Bring bread."]] });
  TG("t-family", "grp", "Family", { av: "family", start: 1, ms: [["rosa", 200, "Photos from the weekend 💛"], ["me", 190, "Lovely!"]] });
  DM("t-mara", "telegram", "mara", { seen: 1, start: 1, ms: [["mara", 300, "Can you send me the slides?"], ["me", 280, "Sent. Shout if the fonts are off."]] });
  TG("t-daily", "ann", "Lichen Daily", { u: 4, nw: 1, ro: 1, ms: [["daily", 2000, "Xanthoria parietina: the orange one on every wall by the sea."], ["daily", 600, "Snow line is down to 1,200 m. Crustose lichens love it.", { r: R("👍14 🔥6") }]] });

  cv("m-oncall", "teams", "grp", "On-call handover", { av: "oncall", start: 1, line: ["dmitri", 7, "Pager is yours from 18:00"] });
  DM("m-ana", "teams", "ana", { u: 1, nw: 1, start: 1, ms: [["me", 300, "Can we move the 1:1?"], ["ana", 12, "Yes, Thursday 10:00 works."]] });
  DM("m-flows", "teams", "flows", { app: 1, tag: "App", u: 1, nw: 1, start: 1, ms: [["flows", 35, "Daily standup reminder: post your update in #engineering."]] });
  cv("m-guild", "teams", "grp", "Platform guild", { av: "platform-guild", start: 1, ms: [["esi", 40, "Notebook for the latency analysis is up."]] });
  DM("m-ben", "teams", "ben", { seen: 1, start: 1, ms: [["ben", 90, "Is the rollout plan final?"], ["me", 60, "Final. Sharing it in Releases."]] });
  cv("m-lunch", "teams", "grp", "Ana, Ben, +3", { duo: ["ana", "ben"], start: 1, ms: [["ben", 200, "Lunch Friday?"], ["dmitri", 150, "In."], ["me", 140, "In."]] });
  cv("m-sprint", "teams", "grp", "Sprint review", { glyph: "calendar", start: 1, ms: [["ana", 2 * DAY, "Recording and notes are in the meeting tab."]] });
  cv("m-notes", "teams", "dm", "Notes", { peer: "me", start: 1, ms: [["me", 500, "Ask Ben about the canary metrics"]] });
  cv("eng/General", "teams", "ch", "General", { sp: "eng", u: 1, nw: 1, start: 1, ms: [["ana", 300, "Release 4.2 is cut. Please test your areas on staging today.", { th: "rel" }], ["esi", 60, "Data export job moved to 02:00 UTC."]] });
  cv("eng/Releases", "teams", "ch", "Releases", { sp: "eng", start: 1, ms: [["me", 55, "Rollout plan for 4.2 is attached."]] });
  cv("eng/Incidents", "teams", "ch", "Incidents", { sp: "eng", start: 1 });
  cv("social/General", "teams", "ch", "General", { sp: "social", start: 1, ms: [["chloe", 2 * DAY, "Pottery class signup closes Friday."]] });
  cv("social/Random", "teams", "ch", "Random", { sp: "social", start: 1 });
  T.rel = { ms: [["ben", 280, "Payments look fine."], ["dmitri", 250, "Alerting dashboards updated."]].map(msg), nw: 1, faces: ["dmitri", "ben"], u: 1 };

  cv("g-climb", "signal", "grp", "Climbing Sundays", { av: "climbing-sundays", u: 1, nw: 1, start: 1, ms: [["sam", 420, "Gym or crag this week?"],
    ["leila", 400, "Crag, if the rock is dry by Sunday. Forecast looks good.", { ed: 1, r: R("👍3*") }], ["lena", 120, "I can drive. Four seats."],
    ["sam", 26, "@Robin Vale you're on rope duty 🧗", { rp: ["sam", "Gym or crag this week?"] }], ["me", 21, "Rope is packed. And the spare harness.", { r: R("🙌2 🔥1") }], ["leila", 4, "Meet at the car park at 9?"]] });
  DM("g-theo", "signal", "theo", { u: 2, nw: 2, start: 1, ms: [["theo", 190, "Are you still up for the coast walk on Sunday?"], ["me", 182, "Yes! Leaving early?"],
    ["theo", 34, "8:00 from the station. I'll bring the coffee.", { r: R("❤️1*") }], ["theo", 9, "Early enough to beat the crowds 😄", { rp: ["me", "Yes! Leaving early?"] }], ["theo", 8, "Low tide is at 10:40, so we can do the long beach."]] });
  DM("g-mara", "signal", "mara", { start: 1, ms: [["mara", 300, "Dinner Thursday?"], ["me", 290, "Thursday works. 7?"], ["mara", 285, "7 it is. I'll book the place by the river.", { r: R("👍1*") }]] });
  cv("g-flat", "signal", "grp", "Flat 3B", { av: "flat-3b", start: 1, ms: [["leila", 900, "Who's home for the delivery tomorrow?"], ["me", 880, "Me, from noon."], ["lena", 870, "Thank you!! Leaving a key under the plant."]] });
  DM("g-elio", "signal", "elio", { start: 1, ms: [["elio", 2900, "Ciao Robin! Did the photos of the garden arrive?"], ["me", 2880, "They did. The tomatoes look amazing."], ["elio", 2870, "Next year even bigger 🍅"]] });

  DM("x-chloe", "messages", "chloe", { u: 2, nw: 2, start: 1, ms: [["chloe", 1520, "Flight's booked! Landing Saturday afternoon"], ["me", 1512, "Yes!! I'll come and get you"],
    ["chloe", 1508, "You're the best. I owe you dinner 🍜"], ["me", 1505, "Holding you to that"], ["chloe", 140, "Boarding now ✈️"], ["me", 138, "Safe flight! I'll be at arrivals."],
    ["chloe", 11, "Landed! Waiting for my bag"], ["chloe", 2, "Which door are you at?"]] });
  cv("x-market", "messages", "grp", "Saturday Market", { u: 1, nw: 1, start: 1, ms: [["ben", 600, "Same stall as last week?"], ["esi", 590, "Stall 14, by the bakery. I'll bring the cart."],
    ["me", 560, "I've got the float and the card reader."], ["ben", 38, "Setting up at 7. Coffee's on me ☕"]] });
  DM("x-rafael", "messages", "rafael", { seen: 0, start: 1, ms: [["rafael", 400, "Still good to borrow the ladder on Saturday?"], ["me", 380, "Of course. It's by the porch."]] });
  DM("x-pharmacy", "messages", "pharmacy", { start: 1, ms: [["pharmacy", 1500, "Your prescription is ready for pickup at Riverside Pharmacy. Reply STOP to opt out."]] });

  // What each network's own sidebar lists, in its order.
  var LISTS = {
    slack: [["Company", "announcements,general,random"], ["Engineering", "eng-incidents,eng-platform,eng-deploys,·1 quiet channel"], ["Product", "design-crit,customer-voice"], ["Archive", "·2 quiet channels"]],
    telegram: ["t-tg", "t-myco", "t-forage", "t-scam", "t-weather", "t-lena", "t-rosa", "t-family", "t-mara", "t-daily"],
    folders: { Work: ["t-ines", "t-myco", "t-forage", "t-lena", "t-rosa", "t-mara"], Family: ["t-rosa", "t-family"], Channels: ["t-daily"] },
    teams: ["m-oncall", "m-ana", "m-flows", "m-guild", "m-ben", "m-lunch", "m-sprint", "m-notes"],
    signal: ["g-climb", "g-theo", "g-mara", "g-flat", "g-elio"],
    messages: ["x-chloe", "x-market", "x-rafael", "x-pharmacy"],
    dms: ["d-ines", "d-carpool", "d-tomasz", "d-mirela", "d-yusuf", "d-odalys", "d-kofi"]
  };

  // ---- What a row says ----
  function last(c) { var m = c.ms[c.ms.length - 1]; return m ? m : c.line ? { w: c.line[0], a: c.line[1], t: c.line[2] } : null; }
  function first(w) { return w === "me" ? "You" : P[w][0].split(" ")[0]; }
  function direct(c) { return c.kind === "dm" || c.kind === "grp"; }
  function badge(c) {
    if (c.mt > 0 && (c.kind !== "dm" || c.muted)) return h("span.pill", null, c.mt);
    if (c.muted || !c.u) return null;
    return direct(c) && !c.app ? h("span.count", null, c.u) : h("span.dot");
  }
  function attention(c) { return c.mt > 0 ? c.mt : direct(c) && !c.app && !c.muted ? c.u : 0; }
  function title(c) { return c.kind !== "dm" && c.kind !== "grp" && c.net !== "telegram" && c.net !== "teams" ? "#" + c.title : c.title; }
  function spaceName(c) { return c.sp ? SP[c.sp][0] : ""; }

  // ---- Avatars ----
  var HUES = [8, 32, 95, 150, 185, 215, 265, 325];
  function tinted(el, name) {
    var n = 0;
    for (var i = 0; i < name.length; i++) n = (n * 31 + name.charCodeAt(i)) >>> 0;
    var hue = HUES[n % 8];
    el.style.background = "hsl(" + hue + " 30% 22%)";
    el.style.color = "hsl(" + hue + " 70% 82%)";
  }
  function letters(name, two) {
    var w = name.replace(/[^\p{L}\s]/gu, " ").trim().split(/\s+/);
    return (w[0] ? w[0][0] : "") + (two && w[1] ? w[1][0] : "");
  }
  function face(name, file, size, place, pres) {
    var el = h("span.av.s" + size + (place ? ".pl" : ""), { "aria-hidden": "true" });
    if (file) put(el, h("img", { src: "assets/demo/" + file + ".webp", alt: "", loading: "lazy" }));
    else { tinted(el, name); put(el, letters(name, size >= 32)); }
    if (pres) put(el, h("span.pres." + (pres === "b" ? "bz" : pres)));
    return el;
  }
  function person(w, size, n) { var p = P[w]; return face(p[0], p[1], size, p[4], n && PRESENT[n] ? p[2] : ""); }
  function duo(a, b, size) {
    var box = h("span.av.s" + size, { "aria-hidden": "true" }), s = size >= 32 ? 20 : 14;
    box.style.background = "none";
    [a, b].forEach(function (w, i) {
      var f = face(P[w][0], "", s), o = size - s;
      f.style.position = "absolute"; f.style.left = i * o + "px"; f.style.top = i * o + "px";
      f.style.fontSize = Math.round(s * 0.45) + "px";
      if (i) f.style.boxShadow = "0 0 0 2px #121315";
      put(box, f);
    });
    return box;
  }
  function glyphFace(name, size, place) {
    var el = h("span.av.s" + size + (place ? ".pl" : ""), { "aria-hidden": "true" }, ic(name, size >= 32 ? 16 : 14));
    return el;
  }
  function convFace(c, size, big) {
    if (c.peer) { var p = P[c.peer]; return face(p[0], p[1] && (big && /ines|chloe/.test(c.peer) ? p[1] + "-l" : p[1]), size, p[4] && c.net !== "telegram", PRESENT[c.net] && c.peer !== "me" && size !== 24 && size < 80 ? p[2] : ""); }
    if (c.glyph) return glyphFace(c.glyph, size, c.glyph !== "bookmark");
    if (c.duo) return duo(c.duo[0], c.duo[1], size);
    if (c.kind === "grp" || c.kind === "forum" || (c.net === "telegram" && c.kind === "ann")) return face(c.title, c.av && (big && /lisbon|sundays|oncall/.test(c.av) ? c.av + "-l" : c.av), size, false);
    if (c.kind === "voice") return glyphFace("volume-2", size, true);
    return c.sp && size < 80 ? face(SP[c.sp][0], SP[c.sp][1], size, true) : glyphFace("hash", size, true);
  }

  // ---- State ----
  var S = { mode: "inbox", filter: "unread", open: "moss/general", keep: "moss/general", thread: null, call: null, folder: "All",
    drafts: {}, reply: null, sw: null, pick: null, lastFocus: null, armed: false, teamsOpen: { eng: 1 } };
  var win, rail, side, pane, right, timer = null, readTimer = null;

  function inboxRows() {
    var rows = Object.keys(C).map(function (k) { return C[k]; }).filter(function (c) {
      return (direct(c) && !c.app && c.kind !== "forum" && !c.sp) || c.inbox || (c.net === "slack" && direct(c));
    });
    rows.sort(function (a, b) { return last(a).a - last(b).a || b.n - a.n; });
    return rows;
  }
  function counts() {
    var rows = inboxRows(), un = 0, me = 0, att = 0, men = false;
    rows.forEach(function (c) { if (c.muted) return; if (c.u || c.mt) un++; if (c.mt) { me++; men = true; } att += attention(c); });
    return { unread: un, mentions: me, attention: att, mention: men };
  }

  // ---- Rail ----
  function railButton(label, content, mode, dot) {
    return h("button.rb", { type: "button", "data-k": "rail-" + mode, "aria-label": label, title: label, "aria-current": S.mode === mode || (mode === "discord" && /^space:/.test(S.mode)) ? "true" : null,
      on: { click: function () { go(mode); } } }, content, dot);
  }
  function renderRail() {
    var k = counts(), kids = [];
    kids.push(railButton("Inbox", ic("inbox", 20), "inbox", k.attention ? h("span.rpill" + (k.mention ? "" : ".q"), null, k.attention > 99 ? "99+" : k.attention) : null));
    kids.push(railButton("People", ic("users", 20), "people", h("span.rdot")));
    kids.push(h("span.rule"));
    NETS.forEach(function (n) {
      var u = false, a = false;
      for (var id in C) { var c = C[id]; if (c.net !== n) continue; if (c.u && !c.muted) u = true; if (attention(c)) a = true; }
      kids.push(railButton(NAMES[n], net(n, 24), n, u || a ? h("span.rdot" + (a ? ".acc" : "")) : null));
    });
    var own = h("span.own", null, person("me", 32, "discord"), h("span.gear", null, ic("settings", 12)));
    kids.push(h("div.rfoot", null, h("span.plus", { "aria-hidden": "true" }, ic("plus", 16)), own));
    keepFocus(function () { rail.replaceChildren.apply(rail, kids); });
  }

  // ---- Sidebar ----
  function seg(choices, pick) {
    return h("div.seg", pick ? { role: "group" } : { "aria-hidden": "true" }, choices.map(function (ch, i) {
      return pick ? h("button", { type: "button", "aria-pressed": ch[2] ? "true" : "false", on: { click: function () { pick(i); } } }, ch[0], ch[1] ? h("small", null, ch[1]) : null)
        : h("span" + (ch[2] ? ".lit" : ""), null, ch[0], ch[1] ? h("small", null, ch[1]) : null);
    }));
  }
  function jumpStrip() {
    return h("div.jumps", null, h("button.jump", { type: "button", on: { click: function () { openSwitcher(); } } }, ic("search", 14), h("span", null, "Jump to…"), h("span.key", null, "⌘K")),
      h("span.ib", { "aria-hidden": "true" }, ic("square-pen", 16)));
  }
  function sec(label, firstOne) { return h("div.sec" + (firstOne ? ".first" : ""), null, label); }
  function row(c, look) {
    var sel = S.open === c.id, m = last(c), unread = c.u > 0 && !c.muted;
    var l1 = h("span.l1", null, look.mark ? net(c.net, 14) : null);
    var showCtx = look.ctx && c.sp && (!direct(c) || c.net === "slack");
    var name = h("span.nm" + (unread ? ".b" : "") + (showCtx ? "" : ".x"), null, title(c));
    put(l1, name);
    if (c.mark) put(l1, h("span", null, c.mark));
    if (c.tag) put(l1, h("span.tag" + (c.tag === "Scam" ? ".scam" : ""), null, c.tag));
    if (c.verified) put(l1, h("span.ro", null, ic("badge-check", 14)));
    if (c.kind === "forum") put(l1, h("span.ro", null, ic("messages-square", 14)));
    if (showCtx) put(l1, h("span.ctx.x", null, (look.voice ? "" : "· ") + spaceName(c)));
    if (c.ro && c.verified) put(l1, h("span.ro", null, "Read-only"));
    if (m && !look.home && !look.voice) put(l1, h("span.time", null, rowTime(m.a)));
    var l2 = h("span.l2" + (unread ? ".u" : "")), pv = h("span.pv");
    var act = look.home && c.peer && doing(c.peer);
    if (act) { look = { voice: act[1] }; if (act[0]) l2.className += " tc-voice"; put(pv, act[0] ? ic(act[0], 12) : null, h("span.x", null, act[1])); }
    else if (look.voice) { l2.className += " tc-u"; put(pv, h("span.x", null, look.voice)); }
    else if (m) {
      if (m.w === "me" && !c.glyph && /telegram|teams|messages/.test(c.net)) put(pv, ic(c.seen ? "check-check" : "check", 14));
      if (m.w === "me" || c.kind !== "dm") if (c.kind !== "ann" && m.w !== "tg") put(pv, h("span", null, first(m.w) + ":"));
      put(pv, h("span.x", null, m.t.replace(/^@Robin Vale,? /, "")));
    }
    put(l2, pv, badge(c));
    var r = h("button.row" + (sel ? ".sel" : "") + (c.muted ? ".muted" : ""), { type: "button", "data-k": "r-" + c.id, "aria-current": sel ? "true" : null, on: { click: function () { open(c.id); } } },
      h("span.in", null, convFace(c, 32), h("span.lines", null, l1, l2)));
    return r;
  }
  function one(c, glyph, extra) {
    var sel = S.open === c.id, unread = c.u > 0 && !c.muted;
    return h("button.one" + (sel ? ".sel" : "") + (c.muted ? ".muted" : "") + (c.kind === "thread" ? ".sub" : ""), { type: "button", "data-k": "r-" + c.id, "aria-current": sel ? "true" : null,
      on: { click: function () { open(c.id); } } }, h("span.in", null, glyph, h("span.nm.x" + (unread || c.mt ? ".b" : "") + (extra ? ".tight" : ""), null, c.title), extra, extra ? h("span.ctx") : null, badge(c)));
  }
  function channelGlyph(c) {
    var g = c.kind === "ann" ? "megaphone" : c.kind === "voice" ? "volume-2" : c.kind === "forum" ? "messages-square" : c.kind === "thread" ? "corner-down-right" : c.priv ? "lock" : "hash";
    return h("span.glyph", null, ic(g, 16));
  }
  // What a friend is doing, over their last message in the Discord home: in a voice channel,
  // in a call, or an activity.
  function doing(w) {
    for (var id in C) if (C[id].seats && !C[id].more && C[id].seats.indexOf(w) >= 0) return ["volume-2", "In " + C[id].title];
    return { odalys: ["phone", "In a call"], mirela: ["", "Listening to Spotify"] }[w];
  }
  function quiet(words) { return h("div.one", null, h("span.in", null, h("span.glyph", null, ic("ellipsis", 16)), h("span.nm.x", null, words))); }
  function action(icon, words, after, trail, go2) {
    var inner = h("span.in", null, h("span.glyph", null, ic(icon, 16)), h("span.nm.x" + (go2 ? ".b" : ""), null, words), after ? h("span.time", null, after) : null, trail);
    return go2 ? h("button.one", { type: "button", on: { click: go2 } }, inner) : h("div.one", null, inner);
  }
  function inVoice(c) {
    var names = c.seats.map(first).concat(S.call && S.call.id === c.id ? ["Robin"] : []);
    var line = names.join(", ") + (c.more ? " +" + c.more : "");
    var r = row(c, { voice: line, ctx: true });
    var nm = r.querySelector(".tc-nm"), g = ic("volume-2", 14); nm.textContent = c.title; g.setAttribute("class", "tc-voice"); nm.before(g);
    r.querySelector(".tc-av").replaceWith(c.seats.length > 1 ? duo(c.seats[0], c.seats[1], 32) : person(c.seats[0], 32));
    return r;
  }
  function strip() { return h("div.strip", null, Array.prototype.slice.call(arguments)); }
  function titled(n, words) { return strip(net(n, 20), h("span.title.x", null, words), h("span.ib", { "aria-hidden": "true" }, ic("ellipsis", 16))); }

  function renderSide() {
    var kids = [], list = h("div.list"), mode = S.mode;
    if (mode === "inbox") {
      var k = counts();
      kids.push(strip(h("span"), seg([["All", 0, S.filter === "all"], ["Unread", k.unread, S.filter === "unread"], ["Mentions", k.mentions, S.filter === "mentions"]],
        function (i) { S.filter = ["all", "unread", "mentions"][i]; S.keep = S.open; renderSide(); }), h("span.ib", { "aria-hidden": "true" }, ic("list", 16)), h("span.ib", { "aria-hidden": "true" }, ic("arrow-down", 16))));
      var rows = inboxRows(), look = { mark: 1, ctx: 1 };
      if (S.filter === "all") {
        var pins = rows.filter(function (c) { return c.pinned; }), rest = rows.filter(function (c) { return !c.pinned; }), at = null;
        if (pins.length) { put(list, sec("Pinned", 1)); pins.forEach(function (c) { put(list, row(c, look)); }); }
        rest.forEach(function (c) {
          var d = dayOf(last(c).a), label = d === 0 ? "Today" : d === 1 ? "Yesterday" : "Earlier";
          if (label !== at) { put(list, sec(label, !pins.length && at === null)); at = label; }
          put(list, row(c, look));
        });
      } else {
        rows.filter(function (c) { return c.id === S.keep || !c.muted && (S.filter === "mentions" ? c.mt > 0 : c.mt > 0 || c.u > 0); })
          .sort(function (a, b) { return (b.mt > 0) - (a.mt > 0) || last(a).a - last(b).a; })
          .forEach(function (c) { put(list, row(c, look)); });
      }
    } else if (mode === "people") {
      kids.push(strip(h("span.title", null, "People")));
      put(list, sec("Online", 1));
      ["ines", "tomasz", "mirela", "yusuf", "mara", "dev", "ben", "lena"].forEach(function (w) {
        var c = C["d-" + w] || C["s-" + w] || C["m-" + w] || C["t-" + w], p = P[w];
        put(list, h("button.row", { type: "button", "data-k": "p-" + w, on: { click: function () { open(c.id); } } },
          h("span.in", null, person(w, 32, c.net), h("span.lines", null, h("span.l1", null, net(c.net, 14), h("span.nm.x", null, p[0])), h("span.l2", null, h("span.pv.x", null, p[3] || "Online"))))));
      });
    } else if (mode === "discord") {
      kids.push(strip(net("discord", 20), h("span.title.tight", null, "Discord"), h("span.ib", { "aria-hidden": "true" }, ic("chevron-down", 12)), seg([["All", 0, 1], ["Unread", 9, 0]])));
      put(list, action("users", "Friends", "16 online", h("span.count", null, 1), function () { go("people"); }), action("mail", "Message requests", "1", h("span.glyph", null, ic("chevron-right", 14))));
      put(list, sec("In voice"));
      ["moss/Quiet room", "moss/Campfire", "moss/Field recording"].forEach(function (id) { put(list, inVoice(C[id])); });
      put(list, sec("Direct messages"));
      LISTS.dms.forEach(function (id) { put(list, row(C[id], { home: 1 })); });
      put(list, action("chevron-down", "Servers"));
      Object.keys(TREES).forEach(function (sp) {
        var att = 0, un = 0;
        for (var id in C) if (C[id].sp === sp) { att += C[id].mt; if (C[id].u && !C[id].muted) un = 1; }
        put(list, h("button.one", { type: "button", on: { click: function () { go("space:" + sp); } } },
          h("span.in", null, face(SP[sp][0], SP[sp][1], 20, true), h("span.nm.x", null, SP[sp][0]), att || un ? h("span.dot" + (att ? ".acc" : "")) : null)));
      });
    } else if (/^space:/.test(mode)) {
      var sp = mode.slice(6);
      kids.push(h("div.strip", null, h("button.ib", { type: "button", "aria-label": "Back to Discord", on: { click: function () { go("discord"); } } }, ic("chevron-left", 16)),
        face(SP[sp][0], SP[sp][1], 20, true), h("span.title.x.tight", null, SP[sp][0]), h("span.ib", { "aria-hidden": "true" }, ic("chevron-down", 12))));
      kids[0].style.paddingLeft = "10px";
      TREES[sp].forEach(function (cat, i) {
        if (cat[0]) put(list, sec(cat[0], !i));
        cat[1].forEach(function (id) {
          var c = C[id];
          // A muted channel with nothing for me is left out, as the app lists them.
          if (c.muted && !c.mt) return;
          put(list, one(c, channelGlyph(c), c.kind === "ann" ? h("span.ro", null, "Read-only") : null));
          if (c.kind === "voice") (c.seats || []).concat(S.call && S.call.id === id ? ["me"] : []).forEach(function (w) {
            put(list, h("div.occ", null, person(w, 16), P[w][0]));
          });
        });
      });
    } else if (mode === "slack") {
      kids.push(strip(face("Harbor Labs", "logo-harbor-labs", 20, true), h("span.title.x.tight", null, "Harbor Labs"), h("span.ib", { "aria-hidden": "true" }, ic("chevron-down", 12)),
        h("span.pill", null, 1), seg([["All", 0, 1], ["Unread", 9, 0]])));
      put(list, action("messages-square", "Threads"));
      LISTS.slack.forEach(function (s, i) {
        put(list, sec(s[0]));
        s[1].split(",").forEach(function (n) { put(list, n[0] === "·" ? quiet(n.slice(1)) : one(C["h/" + n], channelGlyph(C["h/" + n]))); });
      });
      put(list, sec("Direct messages"));
      ["s-mara", "s-maradev", "s-priya", "s-dev", "s-devon"].forEach(function (id) {
        var c = C[id];
        put(list, one(c, c.duo ? duo(c.duo[0], c.duo[1], 20) : person(c.peer, 20, "slack"), c.mark ? h("span", null, c.mark) : null));
      });
    } else if (mode === "telegram") {
      kids.push(titled("telegram", "Telegram"));
      var tabs = h("div.tabs", { role: "group", "aria-label": "Folders" }, [["All"], ["Work", h("span.pill", null, 1)], ["Family", h("span.count", null, 1)], ["Channels", h("span.count", null, 1)]].map(function (t) {
        return h("button", { type: "button", "aria-pressed": S.folder === t[0] ? "true" : "false", on: { click: function () { S.folder = t[0]; renderSide(); } } }, t[0], t[1]);
      }));
      if (S.folder === "All") {
        put(list, action("inbox", "Archive", "Trail Conditions, Old Fl…", h("span.dot")));
        list.lastChild.querySelector(".tc-time").className = "tc-ctx tc-x";
        put(list, sec("Pinned"), row(C["t-saved"], {}), row(C["t-ines"], {}), sec("Chats"));
        LISTS.telegram.forEach(function (id) { put(list, row(C[id], {})); });
      } else LISTS.folders[S.folder].forEach(function (id) { put(list, row(C[id], {})); });
      kids.push(jumpStrip(), tabs);
    } else if (mode === "teams") {
      kids.push(titled("teams", "Teams"));
      put(list, sec("Chats", 1));
      LISTS.teams.forEach(function (id) { put(list, row(C[id], {})); });
      put(list, sec("Teams"));
      ["eng", "social"].forEach(function (sp) {
        put(list, h("button.one", { type: "button", "aria-expanded": S.teamsOpen[sp] ? "true" : "false", on: { click: function () { S.teamsOpen[sp] = !S.teamsOpen[sp]; renderSide(); } } },
          h("span.in", null, face(SP[sp][0], SP[sp][1], 20, true), h("span.nm.x", null, SP[sp][0]), h("span.glyph", null, ic(S.teamsOpen[sp] ? "chevron-down" : "chevron-right", 14)))));
        if (S.teamsOpen[sp]) for (var id in C) if (C[id].sp === sp) put(list, one(C[id], channelGlyph(C[id])));
      });
    } else {
      kids.push(titled(mode, NAMES[mode]));
      LISTS[mode].forEach(function (id) { put(list, row(C[id], {})); });
    }
    if (kids.length < 2) kids.push(jumpStrip());
    kids.push(list);
    if (S.call) kids.push(callBar());
    var scroll = side.querySelector(".tc-list"), top = scroll && side.getAttribute("data-mode") === mode ? scroll.scrollTop : 0;
    keepFocus(function () { side.replaceChildren.apply(side, kids); });
    side.setAttribute("data-mode", mode);
    list.scrollTop = top;
  }

  // ---- The call bar ----
  function callTime() { var s = S.call.t, m = Math.floor(s / 60); return m + ":" + (s % 60 < 10 ? "0" : "") + (s % 60); }
  function callBar() {
    var c = C[S.call.id], k = S.call, lead = c.kind === "voice" ? h("span.vc", null, ic("volume-2", 14)) : convFace(c, 28);
    var ctl = function (key, icon, label, cls, f) { return h("button" + cls, { type: "button", "aria-label": label, title: label, "aria-pressed": cls ? "true" : "false", "data-k": "call-" + key, on: { click: f } }, ic(icon, 16)); };
    var off = k.mute || k.deaf;
    return h("div.call", null, h("div.callc", { role: "group", "aria-label": "Call" },
      h("div.callt", null, lead, h("span.x", null, h("b.x", null, c.title), h("small", null, net("discord", 12), (c.sp ? SP[c.sp][0] : "Discord") + " · ", h("span", { "data-clock": "" }, callTime())))),
      h("div.ctl", null,
        ctl("mute", off ? "mic-off" : "mic", off ? "Unmute" : "Mute", off ? ".off" : ".talk", function () { if (k.deaf) k.deaf = k.mute = false; else k.mute = !k.mute; renderSide(); }),
        ctl("deaf", k.deaf ? "headphone-off" : "headphones", k.deaf ? "Undeafen" : "Deafen", k.deaf ? ".off" : "", function () { k.deaf = !k.deaf; renderSide(); }),
        ctl("cam", "video", k.cam ? "Stop camera" : "Camera", k.cam ? ".on" : "", function () { k.cam = !k.cam; renderSide(); }),
        ctl("share", "monitor-up", k.share ? "Stop sharing" : "Share screen", k.share ? ".on" : "", function () { k.share = !k.share; renderSide(); }),
        ctl("gear", "settings", "Microphone and speaker", k.gear ? ".on" : "", function () { k.gear = !k.gear; renderSide(); }),
        h("button.end", { type: "button", "aria-label": "Leave", title: "Leave", on: { click: leave } }, ic("phone-off", 16)))));
  }
  function join(id) {
    S.call = { id: id, t: 0 };
    clearInterval(timer);
    timer = setInterval(function () {
      if (!S.call) return;
      S.call.t++;
      var el = win.querySelector("[data-clock]"); if (el) el.textContent = callTime();
    }, 1000);
    renderSide(); renderPane();
  }
  function leave() { clearInterval(timer); S.call = null; renderSide(); renderPane(); }

  // ---- The conversation ----
  var MENTION = /(@Robin Vale|@everyone|@channel|@Field Team|@Tomasz Brandt|#trail-cams|#eng-incidents)/;
  function body(m, conv) {
    var el = h("div.body"), mine = /@Robin Vale/.test(m.t);
    m.t.split(MENTION).forEach(function (part, i) {
      if (i % 2) put(el, h("span.men" + (part === "@Robin Vale" || (mine && part === "@everyone") ? ".mine" : ""), null, part));
      else put(el, part);
    });
    if (m.ed) put(el, " ", h("span.ed", null, "(edited)"));
    return el;
  }
  function chips(m, where) {
    var row = h("div.reacts");
    m.r.forEach(function (r) {
      put(row, h("button.chip", { type: "button", "aria-pressed": r[2] ? "true" : "false", "data-k": "c-" + m.id + r[0], title: r[2] ? "Take back your reaction" : "React with this too",
        "aria-label": r[0] + " " + r[1] + (r[2] ? ", yours" : ""), on: { click: function () { react(m, r[0], where); } } }, h("span", null, r[0]), r[1]));
    });
    put(row, h("button.chip.add", { type: "button", "aria-label": "Add reaction", title: "Add reaction", "data-k": "ca-" + m.id, on: { click: function (e) { openPicker(m, where, e.currentTarget); } } }, ic("smile-plus", 16)));
    return row;
  }
  function message(m, head, where, thread) {
    var me = /@Robin Vale/.test(m.t) && m.w !== "me", col = h("div.col");
    if (head) {
      var who = P[m.w];
      put(col, h("div.mh", null, h("b.x", null, m.w === "me" ? "You" : who[0]), h("time", null, clock(m.a)),
        who[4] ? h("span.tag", null, "APP") : null, m.pin ? h("span.pin", null, ic("pin", 12), "Pinned") : null));
    }
    put(col, body(m));
    if (m.card) {
      var dl = h("dl");
      for (var i = 0; i < m.card[2].length; i += 2) put(dl, h("div", null, h("dt", null, m.card[2][i]), h("dd", null, m.card[2][i + 1])));
      put(col, h("div.card", null, h("b", null, m.card[0]), h("p", null, m.card[1]), dl, h("small", null, m.card[3])));
    }
    if (m.r && m.r.length) put(col, chips(m, where));
    var th = m.th && !thread && T[m.th];
    if (th) {
      var lastAgo = th.ms[th.ms.length - 1].a, hrs = Math.round(lastAgo / 60);
      put(col, h("button.thr", { type: "button", on: { click: function () { openThread(where, m.th); } } }, h("span.faces", null, th.faces.slice(0, 3).map(function (w) { return person(w, 20); })),
        h("b", null, th.ms.length + " replies"), h("small", null, Math.round(lastAgo) < 1 ? "Last reply just now" : "Last reply " + (hrs ? hrs + "h" : Math.round(lastAgo) + "m") + " ago"), th.u ? h("i", null, th.u + " new") : null));
    }
    var gut = head ? h("div.gut", null, person(m.w, thread ? 28 : 36)) : h("div.gut.t", null, clock(m.a).replace(/ [AP]M/, ""));
    var tools = h("div.tools", { role: "toolbar", "aria-label": "Message actions" },
      ["👍", "❤️", "😂"].map(function (e) { return h("button", { type: "button", "aria-label": "React with " + e, "data-k": "q-" + m.id + e, on: { click: function () { react(m, e, where); } } }, e); }),
      h("hr"), h("button", { type: "button", "aria-label": "Add reaction", title: "Add reaction", "data-k": "a-" + m.id, on: { click: function (e) { openPicker(m, where, e.currentTarget); } } }, ic("smile-plus", 16)),
      h("button", { type: "button", "aria-label": "Reply", title: "Reply", on: { click: function () { S.reply = { where: where, w: m.w, t: m.t }; renderDock(where); } } }, ic("reply", 16)),
      th ? h("button", { type: "button", "aria-label": "Open thread", title: "Open thread", on: { click: function () { openThread(where, m.th); } } }, ic("messages-square", 16)) : null);
    var rp = m.rp && h("div.reply", null, h("span.elbow"), m.rp[1] ? [person(m.rp[0], 16), h("b", null, m.rp[0] === "me" ? "You" : P[m.rp[0]][0]), h("span.x", null, m.rp[1])] : h("span.gone", null, "Original message was deleted"));
    return h("div.m" + (head ? ".h" : "") + (me ? ".me" : ""), { "data-m": m.id }, rp, h("div.mr", null, gut, col), tools);
  }
  // The messages with their day lines, the New line and grouping: one sender within five
  // minutes is one group, as conversation/rows.rs has it.
  function messages(list, ms, nw, where, thread, root) {
    var prev = null, day = -1, cut = nw ? ms.length - nw : -1;
    ms.forEach(function (m, i) {
      var newDay = dayOf(m.a) !== day;
      day = dayOf(m.a);
      if (i === cut && i > 0 || i === 0 && cut === 0) put(list, h("div.new", null, "New"));
      if (newDay && !thread) put(list, h("div.day", null, dayLine(m.a)));
      if (m.sys) { put(list, h("div.sys", null, m.t)); prev = null; return; }
      var head = !prev || prev.w !== m.w || prev.a - m.a > 5 || newDay || i === cut || m.rp;
      if (head && prev) put(list, h("div.gap"));
      put(list, message(m, head, where, thread));
      prev = m;
      if (root && i === 0) { put(list, h("div.reps", null, (ms.length - 1) + " replies")); prev = null; }
    });
  }
  function header(c) {
    var mixed = !c.sp, kids = [h("div.travel", { "aria-hidden": "true" }, h("span", null, ic("chevron-left", 16)), h("span", null, ic("chevron-right", 16)))];
    kids.push(h("div.face", null, direct(c) ? convFace(c, 24) : glyphFace(c.kind === "voice" ? "volume-2" : c.kind === "ann" ? "megaphone" : c.kind === "forum" ? "messages-square" : c.priv ? "lock" : "hash", 24, true)));
    var path = "";
    if (c.kind === "dm" && PRESENT[c.net] && c.peer !== "me") { var p = P[c.peer]; path = { o: "Online", i: "Idle", d: "Do not disturb", b: "Busy", f: "Offline" }[p[2]] || ""; if (p[3]) path += (path ? " · " : "") + p[3]; }
    else if (c.sp && !direct(c)) path = SP[c.sp][0] + (c.g ? " › " + c.g : "") + (c.topic ? " · " + c.topic : "");
    kids.push(h("div.hl", null, mixed ? net(c.net, 14) : null, h("span.title.x", null, c.title), path ? h("span.path.x", null, path) : null));
    if (c.call) {
      var inIt = S.call && S.call.id === c.id;
      kids.push(inIt ? h("button.pillb.leave", { type: "button", on: { click: leave } }, ic("phone-off", 16), "Leave")
        : h("button.pillb", { type: "button", on: { click: function () { join(c.id); } } }, ic(c.kind === "voice" ? "volume-2" : "phone", 16), c.kind === "voice" ? "Join voice" : "Call"));
    }
    var hand = { slack: c.kind === "ch" ? "Open huddle in Slack" : "", teams: "Open in Teams", messages: "Open in Messages" }[c.net];
    if (hand) kids.push( h("span.pillb.handoff", { "aria-hidden": "true" }, net(c.net, 16), hand, ic("arrow-up-right", 14)));
    kids.push(h("div.act", { "aria-hidden": "true" }, c.kind !== "dm" ? h("span.ib", null, ic("users", 16)) : null, h("span.ib", null, ic(c.kind === "dm" ? "user" : "info", 16))));
    return h("div.head", null, kids);
  }
  function startRow(c) {
    var dm = direct(c);
    return h("div.start", null, dm || c.kind === "forum" || c.net === "telegram" ? convFace(c, 80, true) : glyphFace("hash", 80, true), h("h3", null, c.title),
      h("p", null, dm ? "This is the start of your conversation with " + c.title + "." : "This is the start of #" + c.title + "."));
  }
  function renderPane(toBottom) {
    var c = C[S.open];
    if (!c) { pane.replaceChildren(h("div.msgs")); return; }
    var list = h("div.msgs", { role: "log", "aria-label": "Messages in " + title(c), tabindex: "0" }), old = pane.querySelector(".tc-msgs");
    var keepAt = !toBottom && old && pane.getAttribute("data-open") === c.id ? old.scrollTop : null;
    if (c.start || !c.ms.length) put(list, startRow(c));
    if (c.line && !c.ms.length) { put(list, h("div.day", null, dayLine(c.line[1]))); }
    messages(list, c.ms, c.shownNew, c.id, false, false);
    keepFocus(function () { pane.replaceChildren(header(c), list, h("div.dock")); });
    pane.setAttribute("data-open", c.id);
    renderDock(c.id, true);
    list.scrollTop = keepAt == null ? list.scrollHeight : keepAt;
  }
  function locked(c) {
    if (c.kind === "forum") return "Posts in a forum are started in the network's app.";
    if (c.ro || c.kind === "ann") return "You can read " + title(c) + " but not post in it.";
    return "";
  }
  // The composer: Enter or the send button adds the message as yours, here and nowhere else.
  function renderDock(where, quietly) {
    var inThread = where === "thread", c = C[inThread ? S.thread.conv : where], host = inThread ? right.querySelector(".tc-dock") : pane.querySelector(".tc-dock");
    if (!host) return;
    var no = !inThread && locked(c);
    if (no) { host.replaceChildren(h("div.locked", null, no)); return; }
    var key = inThread ? "t-" + S.thread.id : c.id, gif = /discord|telegram/.test(c.net), attach = !/signal|messages/.test(c.net);
    var input = h("input", { type: "text", "aria-label": inThread ? "Reply in thread" : "Message " + c.title, placeholder: inThread ? "Reply in thread" : "Message " + c.title, "data-k": "in-" + key, autocomplete: "off", spellcheck: "false" });
    input.value = S.drafts[key] || "";
    var send = h("button.send", { type: "submit", "aria-label": "Send", title: "Send  ↩" }, ic("send-horizontal", 16));
    var ready = function () { send.classList.toggle("tc-ready", !!input.value.trim()); };
    input.addEventListener("input", function () { S.drafts[key] = input.value; ready(); });
    ready();
    var form = h("form", { on: { submit: function (e) { e.preventDefault(); post(where, input.value); } } },
      attach ? h("span.ib.attach", { "aria-hidden": "true" }, ic("paperclip", 16)) : null, input,
      gif ? h("span.ib.gifb", { "aria-hidden": "true" }, h("span.gif", null, "GIF")) : null, h("span.ib", { "aria-hidden": "true" }, ic("smile", 16)), send);
    var bar = S.reply && S.reply.where === where ? h("div.replying", null, h("span.x", null, "Replying to ", h("b", null, S.reply.w === "me" ? "yourself" : P[S.reply.w][0])),
      h("button.ib", { type: "button", "aria-label": "Cancel reply", on: { click: function () { S.reply = null; renderDock(where); } } }, ic("x", 14))) : null;
    host.replaceChildren(bar || "", h("div.comp" + (attach ? "" : ".noattach"), { on: { mousedown: function (e) { if (e.target === this || e.target.tagName === "FORM") { e.preventDefault(); input.focus(); } } } }, form));
    if (!quietly) input.focus();
  }
  function post(where, text) {
    text = text.trim();
    if (!text) return;
    var inThread = where === "thread", key = inThread ? "t-" + S.thread.id : where;
    var m = msg(["me", 0, text, S.reply && S.reply.where === where ? { rp: [S.reply.w, S.reply.t] } : {}]);
    S.reply = null; S.drafts[key] = "";
    if (inThread) { T[S.thread.id].ms.push(m); renderThread(true); renderPane(); }
    else { var c = C[where]; c.ms.push(m); c.u = c.mt = c.nw = 0; renderPane(true); renderSide(); renderRail(); }
    var input = win.querySelector('[data-k="in-' + key + '"]'); if (input) input.focus();
  }
  function react(m, e, where) {
    m.r = m.r || [];
    var r = m.r.filter(function (x) { return x[0] === e; })[0];
    if (!r) m.r.push([e, 1, true]);
    else { r[1] += r[2] ? -1 : 1; r[2] = !r[2]; if (!r[1]) m.r.splice(m.r.indexOf(r), 1); }
    if (S.thread && where === "thread") renderThread(); else renderPane();
    if (S.thread && where !== "thread") renderThread();
  }

  // ---- The thread pane ----
  function openThread(conv, id) { S.thread = { conv: conv, id: id }; T[id].u = 0; renderThread(true); renderPane(); }
  function renderThread(toBottom) {
    if (!S.thread) { right.hidden = true; right.replaceChildren(); return; }
    right.hidden = false;
    var th = T[S.thread.id], c = C[S.thread.conv], root = c.ms.filter(function (m) { return m.th === S.thread.id; })[0];
    var list = h("div.msgs", { role: "log", "aria-label": "Thread", tabindex: "0" }), old = right.querySelector(".tc-msgs"), at = old ? old.scrollTop : 0;
    messages(list, [root].concat(th.ms), th.shownNew == null ? (th.shownNew = th.nw) : th.shownNew, "thread", true, true);
    keepFocus(function () {
      right.replaceChildren(h("div.strip", null, h("span.title", null, "Thread"), h("button.ib", { type: "button", "aria-label": "Close thread", on: { click: closeThread } }, ic("x", 16))), list, h("div.dock"));
    });
    renderDock("thread", true);
    list.scrollTop = toBottom ? list.scrollHeight : at;
    fit();
  }
  function closeThread() { S.thread = null; renderThread(); fit(); var b = win.querySelector(".tc-thr"); if (b) b.focus(); }

  // ---- Places ----
  function go(mode) {
    S.mode = mode; S.keep = S.open; renderRail(); renderSide();
  }
  function open(id) {
    var c = C[id];
    if (c.opens) { open(c.opens[0]); openThread(c.opens[0], c.opens[1]); c.u = 0; T[c.opens[1]].u = 0; renderSide(); renderRail(); return; }
    if (S.open !== id && S.thread && S.thread.conv !== id) { S.thread = null; renderThread(); }
    S.open = id; S.keep = id; S.reply = null;
    c.shownNew = c.nw;
    renderPane(true); renderSide();
    clearTimeout(readTimer);
    if (S.armed) readTimer = setTimeout(function () { markRead(c); }, 1000);
  }
  function markRead(c) { c.u = 0; c.mt = 0; c.nw = 0; renderSide(); renderRail(); }

  // ---- ⌘K: the switcher ----
  function context(c) {
    if (c.sp && !direct(c)) return SP[c.sp][0] + (c.g ? " › " + c.g : "");
    var k = c.kind === "dm" ? "Direct message" : "Group";
    return c.net === "slack" ? "Harbor Labs · " + k : k;
  }
  function openSwitcher() {
    if (S.sw) return;
    S.lastFocus = document.activeElement;
    S.sw = { q: "", sel: 0, hits: [] };
    var input = h("input", { type: "text", "aria-label": "Jump to a conversation, a person or a server", placeholder: "Jump to a conversation, a person or a server", autocomplete: "off", spellcheck: "false", role: "combobox", "aria-expanded": "true", "aria-controls": "tc-hits" });
    var hits = h("div.hits", { id: "tc-hits", role: "listbox" });
    var sheet = h("div.sw", { role: "dialog", "aria-modal": "true", "aria-label": "Jump to" }, h("div.swh", null, ic("search", 16), input, h("span.key", null, "esc")), hits,
      h("div.swf", null, [["↑↓", "Move"], ["↵", "Open"], ["#", "Channels"], ["@", "People"]].map(function (x) { return h("span", null, h("span.key", null, x[0]), x[1]); })));
    var scrim = h("div.scrim", { on: { mousedown: function (e) { if (e.target === scrim) closeSwitcher(); } } }, sheet);
    function list() {
      var q = S.sw.q.toLowerCase().replace(/^[#@]/, ""), all = Object.keys(C).map(function (k) { return C[k]; }).filter(function (c) { return c.kind !== "thread"; }), out = [];
      if (!q) {
        var needs = inboxRows().filter(function (c) { return attention(c); }).slice(0, 5);
        needs.forEach(function (c, i) { out.push([c, i ? 0 : "Needs you"]); });
        inboxRows().filter(function (c) { return needs.indexOf(c) < 0; }).slice(0, 6).forEach(function (c, i) { out.push([c, i ? 0 : "Recent"]); });
      } else {
        all.filter(function (c) { return c.title.toLowerCase().indexOf(q) >= 0; }).sort(function (a, b) { return a.title.toLowerCase().indexOf(q) - b.title.toLowerCase().indexOf(q) || (last(a) ? last(a).a : 1e9) - (last(b) ? last(b).a : 1e9); })
          .slice(0, 12).forEach(function (c, i) { out.push([c, i ? 0 : "Conversations"]); });
      }
      S.sw.hits = out.map(function (x) { return x[0]; });
      S.sw.sel = Math.min(S.sw.sel, Math.max(0, out.length - 1));
      hits.replaceChildren();
      if (!out.length) put(hits, h("div.none", null, "No one and nothing called “" + S.sw.q + "”"));
      out.forEach(function (x, i) {
        var c = x[0], t = title(c), nm = h("span.nm.x" + (q ? "" : ".all"));
        var at = q ? t.toLowerCase().indexOf(q) : -1;
        if (at >= 0) put(nm, t.slice(0, at), h("b", null, t.slice(at, at + q.length)), t.slice(at + q.length)); else put(nm, t);
        if (x[1]) put(hits, h("div.hsec", { role: "presentation" }, x[1]));
        put(hits, h("button.hit", { type: "button", role: "option", tabindex: "-1", id: "tc-hit-" + i, "aria-selected": i === S.sw.sel ? "true" : "false", on: { click: function () { pick(i); } } },
          convFace(c, 28), nm, h("span.hc", null, net(c.net, 12), h("span.x", null, context(c))), badge(c)));
      });
      input.setAttribute("aria-activedescendant", out.length ? "tc-hit-" + S.sw.sel : "");
    }
    function pick(i) { var c = S.sw.hits[i]; closeSwitcher(true); if (c) { open(c.id); var r = win.querySelector('[data-k="in-' + c.id + '"]'); if (r) r.focus(); } }
    input.addEventListener("input", function () { S.sw.q = input.value; S.sw.sel = 0; list(); });
    input.addEventListener("keydown", function (e) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); var n = S.sw.hits.length; if (n) { S.sw.sel = (S.sw.sel + (e.key === "ArrowDown" ? 1 : n - 1)) % n; list(); var s = hits.querySelector('[aria-selected="true"]'); if (s) s.scrollIntoView({ block: "nearest" }); } }
      else if (e.key === "Enter") { e.preventDefault(); pick(S.sw.sel); }
      else if (e.key === "Tab") e.preventDefault();
    });
    list();
    S.sw.el = scrim;
    win.appendChild(scrim);
    input.focus();
  }
  function closeSwitcher(picked) {
    if (!S.sw) return;
    S.sw.el.remove(); S.sw = null;
    if (!picked && S.lastFocus && win.contains(S.lastFocus)) S.lastFocus.focus();
  }

  // ---- The emoji picker for reactions: a handful from its Smileys & people ----
  var EMOJI = "😀 grinning,😃 smiley,😄 smile,😁 grin,😆 laughing,😅 sweat,🤣 rofl,😂 joy,🙂 slight,😉 wink,😊 blush,😇 innocent,🥰 love,😍 heart eyes,🤩 star,😘 kiss,😋 yum,😛 tongue,🤔 thinking,🤗 hug,😮 open mouth,😢 cry,😭 sob,😤 triumph,👍 thumbsup,👎 thumbsdown,👏 clap,🙌 raised hands,🙏 pray,💪 muscle,👀 eyes,🔥 fire,❤️ heart,💯 hundred,🎉 tada,🚀 rocket,🌿 herb,🍄 mushroom,☕ coffee,✅ check".split(",");
  function openPicker(m, where, anchor) {
    closePicker();
    S.lastFocus = anchor;
    var input = h("input", { type: "text", placeholder: "Search emoji", "aria-label": "Search emoji", autocomplete: "off" });
    var grid = h("div.grid", { role: "grid" });
    function fill() {
      var q = input.value.toLowerCase().trim();
      grid.replaceChildren.apply(grid, EMOJI.filter(function (x) { return !q || x.indexOf(q) > 0; }).map(function (x) {
        var e = x.split(" ")[0];
        return h("button", { type: "button", "aria-label": x.slice(e.length + 1), title: x.slice(e.length + 1), on: { click: function () { closePicker(); react(m, e, where); } } }, e);
      }));
    }
    input.addEventListener("input", fill);
    input.addEventListener("keydown", function (e) { if (e.key === "Enter") { e.preventDefault(); var b = grid.querySelector("button"); if (b) b.click(); } });
    var pick = h("div.pick", { role: "dialog", "aria-label": "Add reaction" }, h("label", null, ic("search", 14), input), h("p", null, "Smileys & people"), grid);
    fill();
    win.appendChild(pick);
    var k = scale(), a = anchor.getBoundingClientRect(), w = win.getBoundingClientRect();
    var x = (a.left - w.left) / k, y = (a.bottom - w.top) / k + 6, ph = pick.offsetHeight;
    if (y + ph > 692) y = (a.top - w.top) / k - ph - 6;
    pick.style.left = Math.max(8, Math.min(x - 160, win.offsetWidth - 328)) + "px";
    pick.style.top = Math.max(8, y) + "px";
    S.pick = pick;
    input.focus();
  }
  function closePicker() { if (!S.pick) return; S.pick.remove(); S.pick = null; if (S.lastFocus && document.contains(S.lastFocus)) S.lastFocus.focus(); }

  // Rebuilding a region keeps the keyboard where it was: whatever had focus is found again
  // by its data-k.
  function keepFocus(fn) {
    var a = document.activeElement, k = a && win.contains(a) && a.getAttribute("data-k");
    fn();
    if (k) { var b = win.querySelector('[data-k="' + k.replace(/["\\]/g, "\\$&") + '"]'); if (b) b.focus({ preventScroll: true }); }
  }

  // ---- Size: 1120 x 700 as the screenshots are; down to 1040 wide, then scaled ----
  function scale() { var t = win.style.transform.match(/scale\(([\d.]+)\)/); return t ? +t[1] : 1; }
  function fit() {
    if (!win) return;
    var avail = stage.clientWidth, w = Math.max(1040, Math.min(1120, avail)), k = Math.min(1, avail / w);
    win.style.width = w + "px";
    win.style.transform = k < 1 ? "scale(" + k + ")" : "";
    stage.style.height = 700 * k + "px";
  }

  function build() {
    win = h("section.win", { "aria-label": "Working model of the tinyclient window" });
    rail = h("nav.rail", { "aria-label": "Places" });
    side = h("div.side");
    pane = h("main.pane");
    right = h("aside.right", { "aria-label": "Thread", hidden: true });
    win.append(h("div.lights", { "aria-hidden": "true" }, h("i"), h("i"), h("i")), rail, side, pane, right);
    // ⌘K and Escape are the model's only while it has the keyboard or was the last thing
    // clicked (Safari does not focus a clicked button), so they never take the page's.
    var inside = false;
    document.addEventListener("pointerdown", function (e) { inside = win.contains(e.target); }, true);
    document.addEventListener("keydown", function (e) {
      if (!inside && !win.contains(document.activeElement)) return;
      if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") { e.preventDefault(); openSwitcher(); }
      else if (e.key === "Escape") {
        if (S.pick) closePicker(); else if (S.sw) closeSwitcher(); else if (S.reply) { var w = S.reply.where; S.reply = null; renderDock(w); }
        else if (S.thread) closeThread(); else return;
        e.preventDefault();
      }
    });
    win.addEventListener("mousedown", function (e) { if (S.pick && !S.pick.contains(e.target)) closePicker(); }, true);
    // The opening conversation is read once the visitor is here, a second after, as the app reads.
    var arm = function () {
      if (S.armed) return;
      S.armed = true;
      var c = C[S.open]; if (c && c.u + c.mt) readTimer = setTimeout(function () { markRead(c); }, 1000);
    };
    win.addEventListener("pointerenter", arm);
    win.addEventListener("focusin", arm);
    C[S.open].shownNew = C[S.open].nw;
    stage.appendChild(win);
    stage.classList.add("tc-live");
    fit();
    renderRail(); renderSide(); renderPane(true);
  }
  function check() {
    if (wide.matches) { if (!win) build(); stage.classList.add("tc-live"); fit(); }
    else { stage.classList.remove("tc-live"); stage.style.height = ""; }
  }
  check();
  if (wide.addEventListener) wide.addEventListener("change", check); else wide.addListener(check);
  window.addEventListener("resize", function () { if (wide.matches) fit(); });
})();
