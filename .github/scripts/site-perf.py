#!/usr/bin/env python3
"""Fills the site's size and memory numbers in from the measurements, so no page carries a
number someone typed.

    site/perf/compare.json   the five official apps and tinyclient, signed in, read together on
                             one Windows desktop by `scripts/perf/measure.ps1 -Mode compare`.
    site/perf/build.json     tinyclient alone on invented data, `-Mode demo`. CI measures the
                             build of every push to main and the Site workflow passes that
                             reading with --build, in place of the committed one.

    site-perf.py             rewrite the marked blocks in site/
    site-perf.py --check     exit 1 if a committed page differs from what the data gives
    site-perf.py --build F   use F as build.json (and copy it into the site that is published)

The blocks are between `perf:<name>` markers in site/index.html, site/docs/performance.html
and site/style.css. Everything between a pair of markers is written by this file: edit here.
When site/perf/compare.json is not yet present, only the `perf:build` block in
site/docs/performance.html is updated from build.json.
docs/perf-measure.md says how the measurements are taken.
"""
import argparse
import datetime
import io
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SITE = os.path.join(ROOT, "site")

# The official apps, in the order the disk bar stacks them. That order is fixed because each
# segment's colour is its place (.vseg.s0 to .s4), and the memory bars sort themselves.
OFFICIAL = ["teams", "signal", "slack", "discord", "telegram"]
SHORT = {"teams": "Teams", "signal": "Signal", "slack": "Slack", "discord": "Discord", "telegram": "Telegram"}
COLOUR = {"teams": "#5b5fc7", "signal": "#3a76f0", "slack": "#4a154b", "discord": "#5865f2", "telegram": "#229ed9"}
MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September",
          "October", "November", "December"]


def size(mb, nbsp=False):
    """374 MB, 1.4 GB."""
    gap = "&nbsp;" if nbsp else " "
    if mb >= 1000:
        text = "%.1f" % (mb / 1000.0)
        return text + gap + "GB"
    return "%d%sMB" % (round(mb), gap)


def day(iso, nbsp=True):
    d = datetime.date.fromisoformat(iso)
    return "%d%s%s %d" % (d.day, "&nbsp;" if nbsp else " ", MONTHS[d.month - 1], d.year)


def times(big, small):
    return "%d×" % round(big / float(small))


def percent(part, whole):
    text = "%.1f" % (100.0 * part / whole)
    return (text[:-2] if text.endswith(".0") else text) + "%"


def load(path):
    with io.open(path, encoding="utf-8") as f:
        return json.load(f)


def validate_build(build):
    apps = build.get("apps") or []
    if not apps:
        sys.exit("site/perf/build.json has no apps entry")
    built = apps[0]
    for key in ("version", "processes", "memory_mb", "working_set_mb", "commit_mb", "disk_mb"):
        if built.get(key) is None:
            sys.exit("site/perf/build.json: missing %s" % key)
    return built


def model(compare, build):
    apps = {a["id"]: a for a in compare["apps"]}
    missing = [i for i in OFFICIAL + ["tinyclient"] if i not in apps]
    if missing:
        sys.exit("site/perf/compare.json has no reading for: " + ", ".join(missing))
    for app in apps.values():
        for key in ("memory_mb", "working_set_mb", "commit_mb", "disk_mb", "processes", "cpu_percent"):
            if app.get(key) is None:
                sys.exit("site/perf/compare.json: %s has no %s" % (app["id"], key))
    ours = apps["tinyclient"]
    five = [apps[i] for i in OFFICIAL]
    total = {k: sum(a[k] for a in five) for k in ("memory_mb", "working_set_mb", "commit_mb", "disk_mb", "processes")}
    total["cpu_percent"] = round(sum(a["cpu_percent"] for a in five), 1)
    built = validate_build(build)
    return {"apps": apps, "ours": ours, "five": five, "total": total, "compare": compare,
            "build": build, "built": built}


def landing(m):
    ours, total = m["ours"], m["total"]
    by_memory = sorted(m["five"], key=lambda a: -a["memory_mb"])
    spoken = ", ".join("%s %s" % (SHORT[a["id"]], size(a["memory_mb"])) for a in by_memory)
    out = []
    out.append('    <p class="lede center reveal"><span class="nowrap-s">The official apps use %s of RAM and %s of disk.</span> '
               '<span class="nowrap-s">tinyclient %s used %s and %s.</span></p>'
               % (size(total["memory_mb"], True), size(total["disk_mb"], True), ours["version"],
                  size(ours["memory_mb"], True), size(ours["disk_mb"], True)))
    out.append('    <h3 class="vs-head reveal">In memory</h3>')
    out.append('    <div class="vs mem reveal" role="img" aria-label="RAM in use while signed in: %s, tinyclient %s %s.">'
               % (spoken, ours["version"], size(ours["memory_mb"])))
    for app in by_memory:
        out += ['      <div class="vs-row">',
                '        <span class="vs-name">%s</span>' % SHORT[app["id"]],
                '        <div class="vs-bar"><span class="vseg %s"></span></div>' % app["id"],
                '        <span class="vs-val">%s</span>' % size(app["memory_mb"]),
                '      </div>']
    out += ['      <div class="vs-row us">',
            '        <span class="vs-name">tinyclient</span>',
            '        <div class="vs-bar"><span class="vseg mine"></span><span class="vs-win">%s less than all five</span></div>'
            % times(total["memory_mb"], ours["memory_mb"]),
            '        <span class="vs-val">%s</span>' % size(ours["memory_mb"]),
            '      </div>',
            '    </div>']
    out.append('    <p class="fine-note center">RAM in use while signed in, all six read together on one %s desktop, %s. '
               'tinyclient is %s. The number is Task Manager&rsquo;s Memory column, summed over each app&rsquo;s processes. '
               'The build from every change is measured again by itself, on invented data: %s for %s, %s. '
               '<a href="docs/performance.html#memory">How we measure →</a></p>'
               % (m["compare"]["os"], day(m["compare"]["measured"]), ours["version"],
                  size(m["built"]["memory_mb"], True), m["built"]["version"], day(m["build"]["measured"])))
    out.append('    <h3 class="vs-head reveal">On disk</h3>')
    out.append('    <div class="vs reveal" role="img" aria-label="The five official apps take %s on disk together; tinyclient takes %s.">'
               % (size(total["disk_mb"]), size(ours["disk_mb"])))
    out += ['      <div class="vs-row">', '        <span class="vs-name">Official apps</span>', '        <div class="vs-bar stack">']
    for n, key in enumerate(OFFICIAL):
        app = m["apps"][key]
        out.append('          <span class="vseg s%d" data-w="%d" title="%s %s"><span class="vseg-l">%s</span></span>'
                   % (n, app["disk_mb"], app["name"], size(app["disk_mb"]), app["name"]))
    out += ['        </div>',
            '        <span class="vs-val">%s</span>' % size(total["disk_mb"]),
            '      </div>',
            '      <div class="vs-row us">',
            '        <span class="vs-name">tinyclient</span>',
            '        <div class="vs-bar"><span class="vseg mine"></span><span class="vs-win">%s smaller</span></div>'
            % times(total["disk_mb"], ours["disk_mb"]),
            '        <span class="vs-val">%s</span>' % size(ours["disk_mb"]),
            '      </div>',
            '    </div>']
    out.append('    <p class="fine-note center">App size on disk, Windows, each app as its vendor&rsquo;s installer left it, %s. '
               'tinyclient is %s, as its installer left it. <a href="docs/performance.html">How we measure →</a></p>'
               % (day(m["compare"]["measured"]), ours["version"]))
    return "\n".join(out)


def bars(m):
    ours, total = m["ours"], m["total"]
    top = max(a["memory_mb"] for a in m["five"])
    out = []
    for n, key in enumerate(OFFICIAL):
        extra = " min-width: 74px;" if n == len(OFFICIAL) - 1 else ""
        out.append(".vseg.s%d { flex: %d 1 0; background: %s;%s animation-delay: .%02ds; }"
                   % (n, m["apps"][key]["disk_mb"], COLOUR[key], extra, n * 8))
    out.append(".vseg.mine { flex: 0 0 %s; min-width: 6px; background: var(--accent); box-shadow: 0 0 18px var(--accent); "
               "border-radius: 0 6px 6px 0; animation-delay: .6s; }" % percent(ours["disk_mb"], total["disk_mb"]))
    out.append(".mem .vseg { flex-grow: 0; flex-shrink: 0; border-right: 0; }")
    for key in ("teams", "slack", "discord", "signal", "telegram"):
        out.append(".mem .vseg.%s { flex-basis: %s; background: %s; }" % (key, percent(m["apps"][key]["memory_mb"], top), COLOUR[key]))
    out.append(".mem .vseg.mine { flex-basis: %s; min-width: 8px; }" % percent(ours["memory_mb"], top))
    return "\n".join(out)


def row(cells, head=None):
    first = '<th scope="row">%s</th>' % head if head is not None else ""
    return "          <tr>%s%s</tr>" % (first, "".join('<td class="num">%s</td>' % c for c in cells))


def named(app):
    return "%s %s" % (app["name"], app["version"]) if app.get("version") else app["name"]


def build_section(m):
    build, built = m["build"], m["built"]
    where = build.get("where") or ("one %s desktop" % build["os"])
    commit = build.get("commit")
    which = "tinyclient %s" % built["version"]
    if commit:
        which += ' at <a href="https://github.com/TinyComms/tinyclient/commit/%s"><code>%s</code></a>' % (commit, commit[:8])
    out = []
    out.append('    <h2 id="build">Every build</h2>')
    out.append("    <p>Each change that reaches <code>main</code> is built for release and measured before this site is published again, "
               "so these numbers are never older than the code. The build runs on invented data, since a build machine has no "
               "account: Discord, two Slack workspaces, Telegram, Teams, Signal and a Tiny server, with the window open for %d seconds "
               "before the reading.</p>" % build.get("settle_seconds", 0))
    out.append('    <div class="table-wrap" tabindex="0" role="region" aria-label="The newest build">')
    out.append("      <table>")
    out.append("        <caption>%s, measured %s on %s.</caption>" % (which, build["measured"], where))
    out.append('        <thead><tr><th scope="col">Processes</th><th scope="col">Memory</th><th scope="col">Working set</th>'
               '<th scope="col">Commit</th><th scope="col">The four programs on disk</th></tr></thead>')
    out.append("        <tbody>")
    out.append(row([str(built["processes"]), size(built["memory_mb"]), size(built["working_set_mb"]), size(built["commit_mb"]), size(built["disk_mb"])]))
    out += ["        </tbody>", "      </table>", "    </div>"]
    out.append("    <p>This reading and the table above are not the same test. Invented data is smaller than five real accounts, and a "
               "build machine draws the window without a graphics card. It is here to show which way each change moves the number. "
               'The reading is <a href="../perf/build.json"><code>build.json</code></a>.</p>')
    return "\n".join(out)


def page(m):
    ours, total, compare = m["ours"], m["total"], m["compare"]
    by_disk = sorted(m["five"], key=lambda a: -a["disk_mb"])
    by_memory = sorted(m["five"], key=lambda a: -a["memory_mb"])
    when = day(compare["measured"])
    out = []
    out.append('    <p class="lede">How big tinyclient is next to the official apps, what it uses while it runs, and how each number was taken. '
               'The comparison was measured on %s, %s. The build from every change is measured again by itself.</p>'
               % (ours["version"], when))
    out.append("")
    out.append('    <h2 id="size">App size on disk</h2>')
    out.append('    <div class="table-wrap" tabindex="0" role="region" aria-label="App size on disk">')
    out.append("      <table>")
    out.append("        <caption>Windows, measured %s: the folder each app&rsquo;s installer left its program in.</caption>" % compare["measured"])
    out.append('        <thead><tr><th scope="col">App</th><th scope="col">Size on disk</th></tr></thead>')
    out.append("        <tbody>")
    for app in by_disk:
        out.append(row([size(app["disk_mb"])], app["name"]))
    out.append(row(["about " + size(total["disk_mb"])], "The five together"))
    out.append(row([size(ours["disk_mb"])], "tinyclient %s, all five networks" % ours["version"]))
    out += ["        </tbody>", "      </table>", "    </div>"]
    out.append("    <p>Method: install each app with its vendor&rsquo;s own Windows installer, then add up the files in the folder its "
               "program runs from. Telegram keeps the account beside its program, and that folder is left out. An app that "
               "keeps an older copy of itself for its updater has that copy counted, because it is on the disk. "
               "tinyclient&rsquo;s figure is the folder its installer makes, with all four programs.</p>")
    out.append("")
    out.append('    <h2 id="memory">Memory while running</h2>')
    out.append("    <p>One reading, all six signed in with a window open, on one %s desktop, %s. Every app had been open long enough to "
               "finish loading. The CPU column covers the %d seconds after the reading.</p>"
               % (compare["os"], when, compare["cpu_seconds"]))
    out.append('    <div class="table-wrap" tabindex="0" role="region" aria-label="Memory while signed in">')
    out.append("      <table>")
    out.append("        <caption>Memory is the private working set, the number Task Manager calls Memory, summed over the app&rsquo;s "
               "processes, the WebView2 processes that belong to it included. Working set also counts memory shared with other "
               "programs. Commit is what the app has asked the system to back. CPU is a percent of one core.</caption>")
    out.append('        <thead><tr><th scope="col">App</th><th scope="col">Processes</th><th scope="col">Memory</th>'
               '<th scope="col">Working set</th><th scope="col">Commit</th><th scope="col">CPU</th></tr></thead>')
    out.append("        <tbody>")

    def cells(a):
        return [str(a["processes"]), size(a["memory_mb"]), size(a["working_set_mb"]), size(a["commit_mb"]), "%s%%" % a["cpu_percent"]]

    for app in by_memory:
        out.append(row(cells(app), named(app)))
    out.append(row(cells(total), "The five together"))
    out.append(row(cells(ours), "tinyclient %s, all five networks" % ours["version"]))
    out += ["        </tbody>", "      </table>", "    </div>"]
    parts = {p["name"]: p["memory_mb"] for p in ours.get("parts", [])}
    pieces = []
    if "tinyclient" in parts:
        pieces.append("the window was %s" % size(parts["tinyclient"], True))
    if "chatd" in parts:
        pieces.append("<code>chatd</code> was %s" % size(parts["chatd"], True))
    if "mediad" in parts:
        pieces.append("<code>mediad</code> was %s" % size(parts["mediad"], True))
    if pieces:
        out.append("    <p>Of tinyclient&rsquo;s %s, %s.</p>" % (size(ours["memory_mb"], True), " and ".join([", ".join(pieces[:-1]), pieces[-1]]) if len(pieces) > 1 else pieces[0]))
    out.append("    <p>The official apps cannot be signed in to by a build machine, so this table is read by hand, with "
               '<a href="https://github.com/TinyComms/tinyclient/blob/main/scripts/perf/measure.ps1"><code>scripts/perf/measure.ps1</code></a>, '
               "and it says which tinyclient it measured. The numbers on this page and on the front page are filled in from "
               '<a href="../perf/compare.json"><code>compare.json</code></a>; nobody types them.</p>')
    out.append("")
    out.append("    <!-- perf:build -->")
    out.append(build_section(m))
    out.append("    <!-- /perf:build -->")
    out.append("")
    return "\n".join(out)


BLOCKS = [
    ("index.html", "<!-- perf:landing -->", "<!-- /perf:landing -->", landing, "    "),
    ("docs/performance.html", "<!-- perf:page -->", "<!-- /perf:page -->", page, "    "),
    ("style.css", "/* perf:bars */", "/* /perf:bars */", bars, ""),
]

BUILD_ONLY_BLOCKS = [
    ("docs/performance.html", "<!-- perf:build -->", "<!-- /perf:build -->", build_section, "    "),
]


def render(text, start, end, body, indent):
    if text.count(start) != 1 or text.count(end) != 1:
        sys.exit("expected one %s ... %s pair" % (start, end))
    head, rest = text.split(start, 1)
    _, tail = rest.split(end, 1)
    return head + start + "\n" + body + "\n" + indent + end + tail


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--build")
    args = parser.parse_args()

    compare_path = os.path.join(SITE, "perf", "compare.json")
    build_path = os.path.join(SITE, "perf", "build.json")
    if args.build:
        build = load(args.build)
        if not args.check:
            with io.open(build_path, "w", encoding="utf-8", newline="\n") as f:
                f.write(json.dumps(build, indent=2) + "\n")
    else:
        build = load(build_path)

    if os.path.exists(compare_path):
        compare = load(compare_path)
        m = model(compare, build)
        blocks = BLOCKS
    else:
        m = {"build": build, "built": validate_build(build)}
        blocks = BUILD_ONLY_BLOCKS

    stale = []
    for rel, start, end, make, indent in blocks:
        path = os.path.join(SITE, *rel.split("/"))
        with io.open(path, encoding="utf-8", newline="") as f:
            raw = f.read()
        crlf = "\r\n" in raw
        text = raw.replace("\r\n", "\n")
        new = render(text, start, end, make(m), indent)
        if new == text:
            continue
        if args.check:
            stale.append(rel)
            continue
        with io.open(path, "w", encoding="utf-8", newline="") as f:
            f.write(new.replace("\n", "\r\n") if crlf else new)
        print("wrote site/" + rel)
    if stale:
        sys.exit("out of date: %s. Run .github/scripts/site-perf.py and commit." % ", ".join("site/" + s for s in stale))


if __name__ == "__main__":
    main()
