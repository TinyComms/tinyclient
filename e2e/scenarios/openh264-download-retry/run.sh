#!/bin/sh
# The OpenH264 download's tests: three tries with a growing pause, words that say what failed
# and how to try again, a cache folder a restart finds, and a broken cached file taken away.
# No network and no devices: nothing here reaches Cisco.
set -eu
cd "$(dirname "$0")/../../.."
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
run() {
    name=$1
    printf '%s\n' "openh264 download: $name"
    if ! cargo test -p mediad --lib "$name" -- --exact --nocapture --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        return 1
    fi
    cat "$log"
    grep -Fq "running 1 test" "$log"
    grep -Fq "test $name ... ok" "$log"
}
run picture::desktop::tests::the_download_is_tried_again_before_it_gives_up
run picture::desktop::tests::the_words_say_what_failed_and_how_to_retry
run picture::desktop::tests::the_codec_is_cached_where_a_restart_finds_it
run picture::desktop::tests::a_broken_cached_file_is_taken_away
printf '%s\n' 'PASS: a failed OpenH264 download is retried, says why and how to try again, and a good one is kept.'
