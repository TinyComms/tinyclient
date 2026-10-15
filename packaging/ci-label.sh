#!/bin/sh
# For the release workflow: prints VERSION (the workspace's) and LABEL, the name its files carry.
# On a tag LABEL is the tag, which must be v<VERSION>; on a branch it is the branch and commit, so a
# trial build is never mistaken for a release.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
    label=$GITHUB_REF_NAME
    if [ "$label" != "v$version" ]; then
        echo "ci-label.sh: tag $label does not match the workspace version $version (Cargo.toml)" >&2
        exit 1
    fi
else
    sha=${GITHUB_SHA:-$(git -C "$root" rev-parse HEAD)}
    label=$(echo "${GITHUB_REF_NAME:-local}" | tr '/' '-')-$(echo "$sha" | cut -c1-7)
fi
echo "VERSION=$version"
echo "LABEL=$label"
