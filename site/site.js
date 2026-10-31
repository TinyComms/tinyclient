// Progressive enhancement only. Every page reads the same without it.
// 1. Names your system on the download button and points it at that build (on Linux, at the
// choice of formats, which it also lists). 2. Adds Copy to code blocks. 3. Drives the hero
// screenshots. 4. Reveals sections on scroll and lights cards under the pointer.
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
    var counter = root.querySelector(".car-count");
    var count = slides.length;
    if (!track || count < 2) return;
    var EVERY = 5500;
    var still = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : { matches: false };
    var index = 0, timer = null, early = null, stopped = still.matches, hovered = false, focused = false, away = false, settle = null;

    root.classList.add("js");
    root.style.setProperty("--car-ms", EVERY + "ms");
    Array.prototype.forEach.call(root.querySelectorAll("[hidden]"), function (el) { el.hidden = false; });
    // The browser makes a scrolling strip a tab stop by itself; the tabs and arrows are the way in.
    track.setAttribute("tabindex", "-1");
    track.removeAttribute("aria-label");
    // The caption under the strip says which slide is showing; the strip itself stays quiet.
    track.removeAttribute("aria-live");

    function show(i) {
      index = i;
      Array.prototype.forEach.call(slides, function (slide, n) {
        slide.setAttribute("aria-label", (n + 1) + " of " + count);
        slide.setAttribute("aria-hidden", n === i ? "false" : "true");
      });
      Array.prototype.forEach.call(tabs, function (tab, n) {
        if (n === i) tab.setAttribute("aria-current", "true"); else tab.removeAttribute("aria-current");
      });
      var cap = slides[i].querySelector("figcaption");
      if (caption && cap) caption.textContent = cap.textContent;
      if (counter) counter.textContent = (i + 1) + " / " + count;
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
      return !stopped && !hovered && !focused && !away && !document.hidden;
    }

    // The next slide loads shortly before it is due, so it never slides in empty and a visitor
    // who never watches the slideshow never downloads it.
    function preload(i) {
      var img = slides[i % count].querySelector("img");
      if (img) img.loading = "eager";
    }

    function restart() {
      clearTimeout(timer);
      clearTimeout(early);
      root.classList.remove("rolling");
      var on = rolling();
      if (caption) caption.setAttribute("aria-live", on ? "off" : "polite");
      if (!on) return;
      void root.offsetWidth; // restart the progress bar's animation
      root.classList.add("rolling");
      early = setTimeout(function () { preload(index + 1); }, EVERY - 1500);
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
    // Scrolled out of view, it stops turning.
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        away = !entries[0].isIntersecting;
        restart();
      }).observe(root);
    }
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

  // 4. Sections fade up as they come into view, and cards follow the pointer with a soft light.
  // Nothing is hidden unless this runs: the stylesheet only hides .reveal under html.rv.
  (function () {
    var still = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    var items = document.querySelectorAll(".reveal");
    if (!still && items.length && "IntersectionObserver" in window) {
      document.documentElement.classList.add("rv");
      var seen = new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          entry.target.classList.add("in");
          seen.unobserve(entry.target);
        });
      }, { rootMargin: "0px 0px -8% 0px", threshold: 0.12 });
      Array.prototype.forEach.call(items, function (el) { seen.observe(el); });
      // Tabbing ahead of the scroll shows what it lands on.
      document.addEventListener("focusin", function (event) {
        var hidden = event.target.closest && event.target.closest(".reveal");
        if (hidden) hidden.classList.add("in");
      });
    }
    // The privacy diagram rests while it is off screen.
    var flow = document.getElementById("flow");
    if (flow && "IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        flow.classList.toggle("paused", !entries[0].isIntersecting);
      }).observe(flow);
    }
    document.addEventListener("pointermove", function (event) {
      var card = event.target.closest && event.target.closest(".spot");
      if (!card) return;
      var box = card.getBoundingClientRect();
      card.style.setProperty("--mx", (event.clientX - box.left) + "px");
      card.style.setProperty("--my", (event.clientY - box.top) + "px");
    }, { passive: true });
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
    { path: "/docs/using.html", title: "Using tinyclient", about: "Shortcuts, updates, and the optional Linux systemd service for chatd." },
    { path: "/docs/build.html", title: "Build from source", about: "Prerequisites, cargo commands, and the demo mode." },
    { path: "/docs/architecture.html", title: "Architecture", about: "chatd, the window, mediad, the sign-in helper, and storage." },
    { path: "/docs/performance.html", title: "Size and performance", about: "Disk and memory next to the official apps, and how each number was measured." },
    { path: "/docs/privacy.html", title: "Privacy and security", about: "What tinyclient stores, which connections it makes, and what it never does." },
    { path: "/docs/networks.html", title: "Networks and terms", about: "Support on Slack, Discord, Telegram, Teams, Signal, Android Messages and a Tiny server." },
    { path: "/docs/contributing.html", title: "Contributing", about: "Roadmap and the rules every change keeps." },
    { path: "/tinyserver", title: "Tiny server", about: "A self-hosted server for one community: text, voice, one picture, and how to run one." }
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

// Screenshot viewer for the Tiny server page. Each picture is already a link to
// the file, so with no script (or no <dialog>) it still opens. With script, a
// click shows it large, and arrows, swipe and Escape move through the set.
(function () {
  if (!window.HTMLDialogElement) return;
  var links = document.querySelectorAll("a.ts-frame");
  if (!links.length) return;
  var frames = Array.prototype.slice.call(links);
  var index = 0;
  var opener = null;
  var startX = null;

  function svg(path, box) {
    var el = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    el.setAttribute("viewBox", box || "0 0 24 24");
    el.setAttribute("aria-hidden", "true");
    var p = document.createElementNS("http://www.w3.org/2000/svg", "path");
    p.setAttribute("d", path);
    el.appendChild(p);
    return el;
  }

  function button(className, label, path) {
    var el = document.createElement("button");
    el.type = "button";
    el.className = className;
    el.setAttribute("aria-label", label);
    el.appendChild(svg(path));
    return el;
  }

  var dialog = document.createElement("dialog");
  dialog.className = "viewer";
  dialog.setAttribute("aria-label", "Screenshot");
  var close = button("viewer-close", "Close", "M18 6 6 18M6 6l12 12");
  var prev = button("viewer-btn viewer-prev", "Previous screenshot", "m15 18-6-6 6-6");
  var next = button("viewer-btn viewer-next", "Next screenshot", "m9 18 6-6-6-6");
  var stage = document.createElement("div");
  stage.className = "viewer-stage";
  var img = document.createElement("img");
  img.alt = "";
  var cap = document.createElement("p");
  cap.className = "viewer-cap";
  cap.setAttribute("aria-live", "polite");
  var title = document.createElement("strong");
  var line = document.createElement("span");
  var count = document.createElement("em");
  count.className = "viewer-count";
  cap.appendChild(title);
  cap.appendChild(line);
  cap.appendChild(count);
  stage.appendChild(img);
  stage.appendChild(cap);
  dialog.appendChild(close);
  dialog.appendChild(prev);
  dialog.appendChild(next);
  dialog.appendChild(stage);
  document.body.appendChild(dialog);

  function captionOf(link) {
    var figure = link.closest("figure");
    var strong = figure && figure.querySelector("figcaption strong");
    var span = figure && figure.querySelector("figcaption span");
    return {
      title: strong ? strong.textContent : "",
      line: span ? span.textContent : (link.querySelector("img") || {}).alt || ""
    };
  }

  function show(i) {
    index = (i + frames.length) % frames.length;
    var link = frames[index];
    var shot = link.querySelector("img");
    var words = captionOf(link);
    img.src = link.getAttribute("href");
    img.width = shot ? shot.width : 1120;
    img.height = shot ? shot.height : 700;
    img.alt = shot && shot.alt ? shot.alt : words.line;
    title.textContent = words.title;
    line.textContent = words.line;
    count.textContent = (index + 1) + " of " + frames.length;
    var ahead = frames[(index + 1) % frames.length];
    if (ahead) { var warm = new Image(); warm.src = ahead.getAttribute("href"); }
  }

  function open(i, from) {
    opener = from || null;
    show(i);
    if (!dialog.open) dialog.showModal();
  }

  close.addEventListener("click", function () { dialog.close(); });
  prev.addEventListener("click", function () { show(index - 1); });
  next.addEventListener("click", function () { show(index + 1); });
  dialog.addEventListener("click", function (event) {
    if (event.target.closest("img, .viewer-cap, button")) return;
    dialog.close();
  });
  dialog.addEventListener("close", function () {
    if (opener) opener.focus();
  });
  dialog.addEventListener("keydown", function (event) {
    if (event.key === "ArrowRight") { event.preventDefault(); show(index + 1); }
    else if (event.key === "ArrowLeft") { event.preventDefault(); show(index - 1); }
  });
  dialog.addEventListener("pointerdown", function (event) {
    if (event.pointerType === "mouse" && event.button !== 0) return;
    startX = event.clientX;
  });
  dialog.addEventListener("pointerup", function (event) {
    if (startX == null) return;
    var dx = event.clientX - startX;
    startX = null;
    if (Math.abs(dx) < 48) return;
    if (event.target.closest("button")) return;
    show(index + (dx < 0 ? 1 : -1));
  });

  frames.forEach(function (link, i) {
    var words = captionOf(link);
    if (words.title) link.setAttribute("aria-label", "Enlarge: " + words.title);
    link.addEventListener("click", function (event) {
      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      open(i, link);
    });
  });
})();
