#!/bin/sh
# For the release workflow: finds the run of another workflow (ci.yml or mobile.yml) that main's
# push started for this commit, waits for it to finish, and prints its id as run=<id>.
# A tag and the main push of the same commit start together, so on a tag the search keeps trying
# for a few minutes before it decides there is no such run. Run by hand on a branch, it looks once.
# A run that fails fails this script, so nothing is released from a commit main's CI rejected.
# With no run to wait for, run= is empty and the caller builds the files itself; with --dispatch
# this script starts the workflow on the ref instead and waits for that run.
set -eu

workflow=${1:?usage: wait-for-run.sh ci.yml|mobile.yml [--dispatch]}
dispatch=${2:-}
sha=${GITHUB_SHA:?wait-for-run.sh is for GitHub Actions}
out=${GITHUB_OUTPUT:?wait-for-run.sh is for GitHub Actions}

if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
    attempts=10
else
    attempts=1
fi

# Prints the id of the newest run matching the arguments, or nothing.
newest() {
    gh run list --workflow "$workflow" --commit "$sha" --limit 1 "$@" \
        --json databaseId --jq '.[0].databaseId // empty'
}

run=
attempt=1
while :; do
    run=$(newest --event push --branch main)
    [ -n "$run" ] && break
    [ "$attempt" -ge "$attempts" ] && break
    echo "wait-for-run.sh: no $workflow run on main for $sha yet (attempt $attempt of $attempts)"
    attempt=$((attempt + 1))
    sleep 20
done

if [ -z "$run" ] && [ "$dispatch" = --dispatch ]; then
    # A minute of slack for the clock difference between this runner and GitHub.
    since=$(date -u -d "@$(($(date +%s) - 60))" +%Y-%m-%dT%H:%M:%SZ)
    echo "wait-for-run.sh: no $workflow run on main for $sha; starting one on $GITHUB_REF_NAME"
    gh workflow run "$workflow" --ref "$GITHUB_REF_NAME"
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        sleep 10
        run=$(gh run list --workflow "$workflow" --commit "$sha" --event workflow_dispatch --limit 5 \
            --json databaseId,createdAt \
            --jq "[.[] | select(.createdAt >= \"$since\")] | .[0].databaseId // empty")
        [ -n "$run" ] && break
    done
    if [ -z "$run" ]; then
        echo "wait-for-run.sh: the $workflow run started on $GITHUB_REF_NAME never appeared" >&2
        exit 1
    fi
fi

if [ -z "$run" ]; then
    echo "wait-for-run.sh: no $workflow run on main for $sha; the release builds everything itself"
    echo "run=" >> "$out"
    exit 0
fi

url=$(gh run view "$run" --json url --jq .url)
echo "wait-for-run.sh: waiting for $workflow run $run ($url)"
# The job's timeout bounds this wait, and a minute between looks keeps two waiting jobs well
# inside the token's hourly API allowance. A watch that drops out early (a network error) is
# resumed, and the conclusion is read back from the finished run rather than taken from gh's
# exit status.
until [ "$(gh run view "$run" --json status --jq .status)" = completed ]; do
    gh run watch "$run" --interval 60 >/dev/null || sleep 60
done
conclusion=$(gh run view "$run" --json conclusion --jq .conclusion)
if [ "$conclusion" != success ]; then
    echo "wait-for-run.sh: $workflow run $run ended $conclusion; $url" >&2
    exit 1
fi
echo "wait-for-run.sh: $workflow run $run succeeded"
echo "run=$run" >> "$out"
