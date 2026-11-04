#!/bin/sh
# The release captain's check for tinyclient. Read AGENTS.md ("Release captain") first.
#
# Prints the cut (every commit on origin/main since the previous vX.Y.Z tag), the next version, and
# every reason it may not ship today, then exits: 0 when it may ship, 1 when something blocks it,
# 2 when the cut cannot be worked out. It changes nothing unless --apply is passed, and an agent
# passes --apply only when Justin asked to ship, cut, or release the client.
#
#   packaging/release-cut.sh                          dry run
#   packaging/release-cut.sh --apply --summary TEXT   release commit, vX.Y.Z tag, one push
#   packaging/release-cut.sh --check-release X.Y.Z    does the GitHub release have its files?
#
# --minor or --major only when Justin named one; the default is a patch. --second-release-today
# and --through-freeze only when Justin said "second release today" or "ship through the freeze";
# the release commit's body records the phrase, and every other check still runs.
#
# "Today" is this machine's local date. The freeze file is packaging/release-freeze.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
freeze=$here/release-freeze
repo=TinyComms/tinyclient

die() { echo "release-cut: $*" >&2; exit 2; }

apply= bump=patch summary= second= through= check=
while [ $# -gt 0 ]; do
    case "$1" in
        --apply) apply=1 ;;
        --minor) bump=minor ;;
        --major) bump=major ;;
        --summary) [ $# -ge 2 ] || die "--summary needs a line"; summary=$2; shift ;;
        --second-release-today) second=1 ;;
        --through-freeze) through=1 ;;
        --check-release) [ $# -ge 2 ] || die "--check-release needs X.Y.Z"; check=$2; shift ;;
        -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
    shift
done

if [ -n "$check" ]; then
    assets=$(gh release view "v$check" -R "$repo" --json assets --jq '.assets[].name') ||
        { echo "release-cut: no GitHub release v$check" >&2; exit 1; }
    missing=0
    for name in tinyclient-windows-x86_64-setup.exe SHA256SUMS; do
        if printf '%s\n' "$assets" | grep -qx "$name"; then
            echo "ok       $name"
        else
            echo "MISSING  $name"
            missing=1
        fi
    done
    exit "$missing"
fi

git() { command git -C "$root" "$@"; }

today=$(date +%F)
git fetch -q --tags origin main || die "cannot fetch origin"
head=$(git rev-parse origin/main)
prev=$(git tag --merged origin/main --list 'v*' --sort=-v:refname | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1 || true)
[ -n "$prev" ] || die "no vX.Y.Z tag on origin/main"
prev_sha=$(git rev-parse "$prev^{commit}")
# The same line packaging/ci-label.sh reads.
version=$(git show origin/main:Cargo.toml | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "cannot read the version in origin/main's Cargo.toml"
next=$(echo "$version" | awk -F. -v b="$bump" '{
    if (b == "major") print $1 + 1 ".0.0"; else if (b == "minor") print $1 "." $2 + 1 ".0"; else print $1 "." $2 "." $3 + 1 }')

if released=$(gh release view "$prev" -R "$repo" --json assets --jq '.assets | length' 2>/dev/null); then
    released="published, $released files"
else
    released="not published"
fi
echo "tinyclient release cut ($( [ -n "$apply" ] && echo apply || echo dry run ))"
echo "today (local)  $today"
echo "origin/main    $(git log -1 --format='%h %s' "$head")"
echo "last tag       $prev at $(git log -1 --format=%h "$prev_sha"), $(git log -1 --date=format-local:%F --format=%cd "$prev_sha"), GitHub release $released"
echo "next           v$next ($bump; Cargo.toml on main is $version)"
echo
count=$(git rev-list --count "$prev_sha..$head")
echo "commits since $prev ($count):"
if [ "$count" -eq 0 ]; then
    echo "  (none)"
else
    git log --format='  %h %ad %s' --date=format-local:%F "$prev_sha..$head"
fi
echo

blocked=0
ok() { echo "  ok     $*"; }
block() { echo "  BLOCK  $*"; blocked=$((blocked + 1)); }
over() { echo "  over   $*"; }

echo "checks:"

# Freeze: one inclusive range per line, YYYY-MM-DD YYYY-MM-DD. A line that does not parse blocks.
if [ ! -f "$freeze" ]; then
    block "freeze: $freeze is missing"
else
    hit= bad= n=0
    while IFS= read -r line || [ -n "$line" ]; do
        n=$((n + 1))
        line=$(printf '%s' "$line" | sed 's/#.*//; s/^[[:space:]]*//; s/[[:space:]]*$//' | tr -d '\r')
        [ -n "$line" ] || continue
        if ! printf '%s\n' "$line" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}[[:space:]]+[0-9]{4}-[0-9]{2}-[0-9]{2}$'; then
            bad="$bad line $n"; continue
        fi
        from=${line%%[[:space:]]*} to=${line##*[[:space:]]}
        f=$(echo "$from" | tr -d -) t=$(echo "$to" | tr -d -) d=$(echo "$today" | tr -d -)
        if [ "$f" -gt "$t" ]; then bad="$bad line $n"; continue; fi
        if [ "$d" -ge "$f" ] && [ "$d" -le "$t" ]; then hit="$from to $to (line $n)"; fi
    done < "$freeze"
    if [ -n "$bad" ]; then
        block "freeze: release-freeze does not parse at$bad"
    elif [ -z "$hit" ]; then
        ok "freeze: $today is not frozen"
    elif [ -n "$through" ]; then
        over "freeze: $today is inside $hit; Justin said \"ship through the freeze\""
    else
        block "freeze: $today is inside $hit"
    fi
fi

today_tags=$(git for-each-ref 'refs/tags/v*' \
    --format='%(refname:short) %(*committerdate:format-local:%Y-%m-%d)%(committerdate:format-local:%Y-%m-%d)' |
    awk -v d="$today" '$2 == d { printf "%s%s", sep, $1; sep = ", " }')
if [ -z "$today_tags" ]; then
    ok "one a day: no v* tag points at a commit made today"
elif [ -n "$second" ]; then
    over "one a day: tags on commits made today: $today_tags; Justin said \"second release today\""
else
    block "one a day: tags on commits made today: $today_tags"
fi

shippable=$(git log --format=%s "$prev_sha..$head" | grep -Evc '^Release [0-9]+\.[0-9]+\.[0-9]+: ' || true)
if [ "$shippable" -gt 0 ]; then
    ok "cut: $shippable commit(s) to ship"
else
    block "cut: nothing on origin/main since $prev but a release commit"
fi

if [ "v$version" = "$prev" ]; then
    ok "version: Cargo.toml on main matches $prev"
else
    block "version: Cargo.toml on main is $version but the last tag is $prev"
fi
if git rev-parse -q --verify "refs/tags/v$next" >/dev/null; then
    block "version: v$next already exists"
fi

ci=$(gh run list -R "$repo" --workflow ci.yml --commit "$head" -L 1 --json status,conclusion,url \
    --jq '.[0] | "\(.status) \(.conclusion) \(.url)"' 2>/dev/null) || ci=
case "$ci" in
    "completed success "*) ok "CI: green on $(git rev-parse --short "$head")" ;;
    "") block "CI: no CI run found for $(git rev-parse --short "$head")" ;;
    *) block "CI: $(git rev-parse --short "$head") is $ci" ;;
esac

dirty=$(git status --porcelain | wc -l | tr -d ' ')
if [ "$dirty" -eq 0 ]; then
    ok "worktree: clean"
else
    block "worktree: $dirty uncommitted or untracked path(s) in $root"
fi
branch=$(git symbolic-ref -q --short HEAD || echo detached)
if [ "$branch" = main ] && [ "$(git rev-parse HEAD)" = "$head" ]; then
    ok "branch: main at origin/main"
else
    block "branch: the release commit goes on an up-to-date main; this checkout is $branch at $(git rev-parse --short HEAD)"
fi

echo
if [ "$blocked" -gt 0 ]; then
    echo "verdict: BLOCKED ($blocked). Nothing was changed."
    exit 1
fi
if [ -z "$apply" ]; then
    echo "verdict: READY for v$next. Dry run: nothing was changed."
    exit 0
fi

# Apply: the release commit, the tag, and one push. Only after every check above passed.
[ -n "$summary" ] || die "--apply needs --summary with one line from the cut"
case "$summary" in *"
"*) die "--summary is one line" ;; esac
echo "verdict: READY. Releasing v$next."
tmp=$(mktemp)
awk -v v="$next" '!done && /^version = "/ { print "version = \"" v "\""; done = 1; next } { print }' \
    "$root/Cargo.toml" > "$tmp"
mv "$tmp" "$root/Cargo.toml"
(cd "$root" && cargo check -q) || die "cargo check failed; Cargo.toml is bumped but nothing is committed"
changed=$(git status --porcelain | awk '{ print $2 }' | sort | tr '\n' ' ')
[ "$changed" = "Cargo.lock Cargo.toml " ] || die "expected only Cargo.toml and Cargo.lock to change, got: $changed"
msg=$(mktemp)
{
    echo "Release $next: $summary"
    if [ -n "$second" ] || [ -n "$through" ]; then
        echo
        [ -z "$second" ] || echo "Justin said \"second release today\"."
        [ -z "$through" ] || echo "Justin said \"ship through the freeze\"."
    fi
} > "$msg"
git add Cargo.toml Cargo.lock
git commit -q -F "$msg"
rm -f "$msg"
git tag "v$next"
git push --atomic origin HEAD:main "v$next"
echo
echo "Pushed $(git log -1 --format='%h %s') and v$next."
echo "Next: watch the Release run, then confirm its files:"
echo "  gh run list -R $repo --workflow release.yml --branch v$next -L 1"
echo "  gh run watch <id> -R $repo --exit-status"
echo "  packaging/release-cut.sh --check-release $next"
