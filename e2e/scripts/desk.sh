#!/bin/sh
# Runs on the desktop this ssh session landed on. Appium stays on 127.0.0.1.
set -eu
if [ -d "$HOME/.local/share/mise/shims" ]; then
  PATH="$HOME/.local/share/mise/shims:$PATH"
  export PATH
fi
cd "$HOME/tiny-call-e2e/e2e"
npm ci
case "$(uname -s)" in
  Darwin)
    node node_modules/appium/index.js driver install mac2 \
      || node node_modules/appium/index.js driver list --installed | grep -q mac2
    ;;
  Linux)
    node node_modules/appium/index.js driver install --source=npm @itsmeaj/appium-linux-driver \
      || node node_modules/appium/index.js driver list --installed | grep -q atspi2
    ;;
  *)
    echo "desk.sh runs on the Mac or the Linux desktop" >&2
    exit 1
    ;;
esac
npm run build
if [ -f "$HOME/.config/tiny-e2e/env" ]; then
  set -a
  # shellcheck disable=SC1091
  . "$HOME/.config/tiny-e2e/env"
  set +a
fi
export TINY_E2E_DATA="$HOME/tiny-call-e2e/e2e/artifacts"
node dist/cli.js "${1:-doctor}"
