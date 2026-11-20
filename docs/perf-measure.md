# The size and memory numbers on the site

No page carries a number someone typed. `site/index.html`, `site/docs/performance.html` and the
bar widths in `site/style.css` are filled in by `.github/scripts/site-perf.py` from two readings,
both taken by `scripts/perf/measure.ps1` on Windows.

| Reading | What it is | Who takes it, and when |
| --- | --- | --- |
| `site/perf/build.json` | tinyclient alone, a release build started on invented data (`chatd --seed-demo`) | CI, for every push to `main`. The Site workflow publishes the site again with it when that CI run ends. The committed file is only what is shown until the first run. |
| `site/perf/compare.json` | The five official apps and the installed tinyclient, all signed in, read together | A person, on a Windows desktop with all six open. A build machine cannot sign in to the official apps. Until this file is committed, `site-perf.py` updates only the "Every build" block from `build.json`. |

The memory number is the private working set, summed over the app's processes: what Task Manager
shows in its Memory column. The WebView2 processes that name an app as their host count as that
app's. Working set and commit are recorded beside it.

## The comparison, by hand

Open Teams, Slack, Discord, Signal, Telegram and tinyclient, signed in, each with its window
open. Leave them a few minutes, with no call running. Then, from the repository:

```powershell
scripts\perf\measure.ps1 -Mode compare -RequireAll -Out site\perf\compare.json
python .github\scripts\site-perf.py
```

Commit the JSON and the three files the second command rewrites, in one pull request.

The reading names the apps, their versions and the Windows release, and nothing else. It never
holds the machine's name, its user, its hardware, or a path. The script refuses to write a
result that contains the machine's or the user's name. Do not add any of that to a page by hand
either: the site says "one Windows 11 desktop".

## The build, in CI

`package (windows)` in `ci.yml` runs the script in `-Mode demo` against `target/release` and
uploads `perf-build`. It runs with `TINYCLIENT_DIR` set to a scratch directory, so it touches no
account and no tinyclient already open. A reading that fails does not fail the job: the site keeps
the last one.

`site.yml` runs on a push that touches `site/`, and when a CI run on `main` ends. It takes the
newest `perf-build` there is and runs `site-perf.py --build` before it publishes.

A build machine draws the window without a graphics card and has no real accounts, so this
number is not comparable with the table beside it. It shows which way a change moves it.

## Changing what the pages say

Everything between a `perf:` marker pair is written by `site-perf.py`. Edit the words there, run
it, and commit the pages with it. `Site check` fails a pull request whose pages differ from what
the script gives.
