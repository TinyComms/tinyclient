// Progressive enhancement only. Every page reads the same without it.
// 1. Names your system on the download button and points it at that build (on Linux, at the
// choice of formats, which it also lists). 2. Adds Copy to code blocks. 3. Drives the hero
// screenshots.
(function () {
  "use strict";

  var systems = {
    mac: { label: "Download for macOS", hint: "For Apple silicon, macOS 13 or later; a disk image", href: "/download/macos" },
    // Linux has a format per distribution, so the button opens the choice and the formats are listed.
    linux: { label: "Download for Linux", hint: "For x86_64 and arm64: .deb, .rpm, AppImage or .tar.gz", href: "/docs/#linux" },
    windows: { label: "Download for Windows", hint: "For x86_64; an installer that needs no administrator", href: "/download/windows" }
  };

  function detect() {
    var platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
    var ua = navigator.userAgent || "";
    var all = (platform + " " + ua).toLowerCase();
    if (/iphone|ipad|ipod|android/.test(all)) return null;
    if (/mac/.test(all)) return "mac";
    if (/win/.test(all)) return "windows";
    if (/linux|x11|cros/.test(all)) return "linux";
    return null;
  }

  var os = detect();
  if (os) {
    var label = document.querySelectorAll("[data-download-label]");
    for (var i = 0; i < label.length; i++) {
      label[i].textContent = systems[os].label;
      var link = label[i].closest("a");
      if (link) link.setAttribute("href", systems[os].href);
    }
    var hint = document.querySelectorAll("[data-download-hint]");
    for (var j = 0; j < hint.length; j++) hint[j].textContent = systems[os].hint;
    if (os === "linux") {
      var choices = document.querySelectorAll("[data-linux-choices]");
      for (var k = 0; k < choices.length; k++) choices[k].hidden = false;
    }
  }

  // 3. The hero screenshots: arrows, tabs, keyboard and auto-advance on top of a scroll-snap
  // strip that works by itself (swipe and scroll are the browser's own).
  (function () {
    var root = document.getElementById("hero-shots");
    if (!root) return;
    var track = root.querySelector(".car-track");
    var slides = root.querySelectorAll(".car-slide");
    var tabs = root.querySelectorAll(".car-tab");
    var tabList = root.querySelector(".car-tabs");
    var caption = root.querySelector(".car-caption");
    var play = root.querySelector(".car-play");
    var count = slides.length;
    if (!track || count < 2) return;
    var EVERY = 5500;
    var still = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : { matches: false };
    var index = 0, timer = null, stopped = still.matches, hovered = false, focused = false, settle = null;

    root.classList.add("js");
    root.style.setProperty("--car-ms", EVERY + "ms");
    Array.prototype.forEach.call(root.querySelectorAll("[hidden]"), function (el) { el.hidden = false; });
    track.removeAttribute("tabindex");
    track.removeAttribute("aria-label");

    function show(i) {
      index = i;
      Array.prototype.forEach.call(slides, function (slide, n) {
        slide.setAttribute("aria-label", (n + 1) + " of " + count);
        slide.setAttribute("aria-hidden", n === i ? "false" : "true");
        // The next one loads before it is needed, so it never slides in empty.
        if (n === (i + 1) % count) {
          var img = slide.querySelector("img");
          if (img) img.loading = "eager";
        }
      });
      Array.prototype.forEach.call(tabs, function (tab, n) {
        if (n === i) tab.setAttribute("aria-current", "true"); else tab.removeAttribute("aria-current");
      });
      var cap = slides[i].querySelector("figcaption");
      if (caption && cap) caption.textContent = cap.textContent;
      // Keep the chosen tab in view in the tab strip, without moving the page.
      var tab = tabs[i];
      if (tab && tabList && tabList.scrollWidth > tabList.clientWidth) {
        var left = tab.parentNode.offsetLeft - (tabList.clientWidth - tab.offsetWidth) / 2;
        tabList.scrollTo({ left: Math.max(0, left), behavior: still.matches ? "auto" : "smooth" });
      }
      restart();
    }

    function go(i) {
      i = (i + count) % count;
      track.scrollTo({ left: slides[i].offsetLeft, behavior: still.matches ? "auto" : "smooth" });
      show(i);
    }

    function rolling() {
      return !stopped && !hovered && !focused && !document.hidden;
    }

    function restart() {
      clearTimeout(timer);
      root.classList.remove("rolling");
      var on = rolling();
      track.setAttribute("aria-live", on ? "off" : "polite");
      if (!on) return;
      void root.offsetWidth; // restart the progress bar's animation
      root.classList.add("rolling");
      timer = setTimeout(function () { go(index + 1); }, EVERY);
    }

    // A swipe or a scroll: whichever slide it settles on is the one shown.
    track.addEventListener("scroll", function () {
      clearTimeout(settle);
      settle = setTimeout(function () {
        var i = Math.round(track.scrollLeft / track.clientWidth);
        if (i !== index && i >= 0 && i < count) show(i);
      }, 90);
    }, { passive: true });

    root.querySelector(".car-prev").addEventListener("click", function () { go(index - 1); });
    root.querySelector(".car-next").addEventListener("click", function () { go(index + 1); });
    Array.prototype.forEach.call(tabs, function (tab, n) {
      tab.addEventListener("click", function (event) { event.preventDefault(); go(n); });
    });
    root.addEventListener("keydown", function (event) {
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      if (event.key === "ArrowLeft") { event.preventDefault(); go(index - 1); }
      else if (event.key === "ArrowRight") { event.preventDefault(); go(index + 1); }
    });
    if (play) {
      play.addEventListener("click", function () {
        stopped = !stopped;
        root.classList.toggle("stopped", stopped);
        play.setAttribute("aria-label", stopped ? "Play the slideshow" : "Pause the slideshow");
        restart();
      });
    }
    root.addEventListener("mouseenter", function () { hovered = true; restart(); });
    root.addEventListener("mouseleave", function () { hovered = false; restart(); });
    root.addEventListener("focusin", function () { focused = true; restart(); });
    root.addEventListener("focusout", function (event) {
      if (!root.contains(event.relatedTarget)) { focused = false; restart(); }
    });
    document.addEventListener("visibilitychange", restart);
    var onMotion = function () {
      stopped = still.matches || stopped;
      root.classList.toggle("stopped", stopped);
      restart();
    };
    if (still.addEventListener) still.addEventListener("change", onMotion);
    root.classList.toggle("stopped", stopped);
    if (play && stopped) play.setAttribute("aria-label", "Play the slideshow");
    show(0);
  })();

  registerSiteTools();

  if (!navigator.clipboard || !window.isSecureContext) return;
  var blocks = document.querySelectorAll(".code pre");
  Array.prototype.forEach.call(blocks, function (pre) {
    var button = document.createElement("button");
    button.type = "button";
    button.className = "copy";
    button.textContent = "Copy";
    button.setAttribute("aria-label", "Copy code");
    button.addEventListener("click", function () {
      // Comments (class "c") are left out; prompts are not written in the blocks.
      var clone = pre.cloneNode(true);
      Array.prototype.forEach.call(clone.querySelectorAll(".c"), function (c) { c.remove(); });
      var text = clone.textContent.replace(/[ \t]+$/gm, "").replace(/\n+$/, "");
      navigator.clipboard.writeText(text).then(function () {
        button.textContent = "Copied";
        button.classList.add("done");
        setTimeout(function () { button.textContent = "Copy"; button.classList.remove("done"); }, 1600);
      });
    });
    pre.parentNode.appendChild(button);
  });
})();

// WebMCP: key actions on the public site. document.modelContext is preferred;
// navigator.modelContext is the fallback older Chrome builds expose.
function registerSiteTools() {
  var ac = new AbortController();
  window.addEventListener("pagehide", function () { ac.abort(); });

  var docs = [
    { path: "/docs/", title: "Getting started", about: "Install on macOS, Linux or Windows, sign in, and see where data is kept." },
    { path: "/docs/build.html", title: "Build from source", about: "Prerequisites, cargo commands, and the demo mode." },
    { path: "/docs/architecture.html", title: "Architecture", about: "chatd, the window, mediad, the sign-in helper, and storage." },
    { path: "/docs/performance.html", title: "Size and performance", about: "Disk and memory next to the official apps, and how each number was measured." },
    { path: "/docs/privacy.html", title: "Privacy and security", about: "What tinyclient stores, which connections it makes, and what it never does." },
    { path: "/docs/networks.html", title: "Networks and terms", about: "Support on Slack, Discord, Telegram, Teams and Signal." },
    { path: "/docs/contributing.html", title: "Contributing", about: "Roadmap and the rules every change keeps." }
  ];

  var tools = [
    {
      name: "list_docs",
      description: "List tinyclient's public documentation pages and what each one covers.",
      inputSchema: { type: "object", properties: {}, additionalProperties: false },
      execute: function () {
        var lines = docs.map(function (page) {
          return page.title + " — " + page.about + " https://tinyclient.io" + page.path;
        });
        return Promise.resolve({ content: [{ type: "text", text: lines.join("\n") }] });
      }
    },
    {
      name: "downloads",
      description: "Return the stable download URLs for tinyclient. There is no account and no license key.",
      inputSchema: {
        type: "object",
        properties: {
          platform: {
            type: "string",
            enum: ["macos", "linux", "windows", "all"],
            description: "Which builds to list. Defaults to all."
          }
        },
        additionalProperties: false
      },
      execute: function (input) {
        var platform = input && input.platform ? input.platform : "all";
        var all = {
          macos: "https://tinyclient.io/download/macos",
          linux: "https://tinyclient.io/docs/#linux",
          windows: "https://tinyclient.io/download/windows"
        };
        var text = platform === "all"
          ? "macOS " + all.macos + "\nLinux " + all.linux + "\nWindows " + all.windows + "\nChecksums https://tinyclient.io/download/checksums"
          : all[platform] || all.macos;
        return Promise.resolve({ content: [{ type: "text", text: text }] });
      }
    },
    {
      name: "open_page",
      description: "Open a tinyclient.io documentation page or the home page in this tab.",
      inputSchema: {
        type: "object",
        properties: {
          path: {
            type: "string",
            description: "Site path, such as /docs/privacy.html or /."
          }
        },
        required: ["path"],
        additionalProperties: false
      },
      execute: function (input) {
        var path = input && input.path ? String(input.path) : "/";
        var allowed = path === "/" || docs.some(function (page) { return page.path === path; });
        if (!allowed) {
          return Promise.resolve({ content: [{ type: "text", text: "That path is not a public tinyclient page." }] });
        }
        window.location.assign(path);
        return Promise.resolve({ content: [{ type: "text", text: "Opening " + path }] });
      }
    }
  ];

  function register(tool) {
    var options = { signal: ac.signal };
    if (document.modelContext && document.modelContext.registerTool) {
      return document.modelContext.registerTool(tool, options);
    }
    if (navigator.modelContext && navigator.modelContext.registerTool) {
      return navigator.modelContext.registerTool(tool, options);
    }
    return null;
  }

  for (var i = 0; i < tools.length; i++) register(tools[i]);
}
