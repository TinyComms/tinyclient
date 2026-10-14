// Progressive enhancement only. Every page reads the same without it.
// 1. Names your system on the download button. 2. Adds Copy to code blocks.
(function () {
  "use strict";

  var systems = {
    mac: { label: "Download for macOS", hint: "For Apple silicon, macOS 13 or later" },
    linux: { label: "Download for Linux", hint: "For x86_64; four binaries in a .tar.gz" },
    windows: { label: "Download for Windows", hint: "For x86_64; four .exe files in a .zip" }
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
    for (var i = 0; i < label.length; i++) label[i].textContent = systems[os].label;
    var hint = document.querySelectorAll("[data-download-hint]");
    for (var j = 0; j < hint.length; j++) hint[j].textContent = systems[os].hint;
  }

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
