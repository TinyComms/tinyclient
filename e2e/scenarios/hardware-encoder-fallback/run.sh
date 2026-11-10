#!/bin/sh
# The Media Foundation hardware encoder's selection and helpers, unit-tested on Windows (the
# module is cfg(windows), so this driver runs where it compiles). No network, no devices.
set -eu
cd "$(dirname "$0")/../../.."
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
run() {
    name=$1
    printf '%s\n' "media foundation encoder: $name"
    if ! cargo test -p mediad --lib "mf_encoder::tests::$name" -- --exact --nocapture --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        return 1
    fi
    cat "$log"
    grep -Fq "running 1 test" "$log"
    grep -Fq "test mf_encoder::tests::$name ... ok" "$log"
    if grep -Eq '(^|[[:space:]])skipped:' "$log"; then
        echo 'FAIL: a fixture skipped' >&2
        return 1
    fi
}
run a_clean_probe_means_hardware_and_any_failure_means_the_download
run nv12_is_the_y_plane_and_the_two_chroma_planes_interleaved
run nv12_of_a_short_buffer_writes_nothing_past_it
run annex_b_already_annex_b_is_untouched
run length_prefixed_units_come_out_behind_start_codes
run a_truncated_or_empty_stream_never_runs_past_itself
run sizes_and_rates_are_packed_width_over_height_and_thousandths
printf '%s\n' 'PASS: the hardware encoder is chosen when present and OpenH264 stays the fallback.'
