#!/bin/sh
# Posts one release note to the #releases Discord webhook.
# DISCORD_WEBHOOK_URL, VERSION, and RELEASE_URL come from the environment.
# A URL copied from Discord's GitHub or Slack integration ends in /github or
# /slack and rejects a Discord message with HTTP 400. Those suffixes are removed.
# The webhook URL is never printed. A missing webhook is not an error.
set -eu
url=$(printf '%s' "${DISCORD_WEBHOOK_URL:-}" | tr -d '[:space:]')
[ -n "$url" ] || exit 0
case "$url" in
    https://discord.com/api/webhooks/*|https://discordapp.com/api/webhooks/*|https://canary.discord.com/api/webhooks/*) ;;
    *)
        echo "release-note: DISCORD_WEBHOOK_URL is not a Discord webhook" >&2
        exit 1
        ;;
esac
url=${url%/}
case "$url" in
    */github)
        url=${url%/github}
        echo "release-note: posting to the Discord webhook, not its /github endpoint"
        ;;
    */slack)
        url=${url%/slack}
        echo "release-note: posting to the Discord webhook, not its /slack endpoint"
        ;;
esac
version=${VERSION:?}
release_url=${RELEASE_URL:?}
content="**tinyclient ${version} is out.** Download: ${release_url} Checksums are in SHA256SUMS on the release page."
payload=$(jq -n --arg content "$content" '{content: $content}')
body=$(mktemp)
code=$(curl -sS -o "$body" -w "%{http_code}" -A "tinyclient-release" -H "Content-Type: application/json" --data-binary "$payload" "$url")
if [ "$code" -lt 200 ] || [ "$code" -ge 300 ]; then
    echo "release-note: Discord returned HTTP ${code}" >&2
    jq -c '{message, code, errors}' "$body" >&2 || true
    exit 1
fi
echo "release-note: posted"
