#!/bin/sh
# The Media Foundation hardware encoder's tests, unit-tested on Windows (the module is
# cfg(windows), so this driver runs where it compiles): the selection logic, the fake-MFT
# seam that holds the open, the profile refusal, the frame loop and the keyframe flag
# without hardware, and the pure NV12/Annex B helpers. No network, no devices, no encoder
# hardware.
set -eu
cd "$(dirname "$0")/../../.."
log=$(mktemp)
trap 'rm -f "$log"' EXIT HUP INT TERM
run() {
    name=$1
    printf '%s\n' "media foundation encoder: $name"
    if ! cargo test -p mediad --lib "$name" -- --exact --nocapture --test-threads=1 >"$log" 2>&1; then
        cat "$log"
        return 1
    fi
    cat "$log"
    grep -Fq "running 1 test" "$log"
    grep -Fq "test $name ... ok" "$log"
    if grep -Eq '(^|[[:space:]])skipped:' "$log"; then
        echo 'FAIL: a fixture skipped' >&2
        return 1
    fi
}
run mf_encoder::tests::a_clean_probe_means_hardware_and_any_failure_means_the_download
run mf_encoder::tests::an_encoder_that_refuses_the_output_type_is_failed_in_its_own_words
run mf_encoder::tests::a_profile_other_than_the_baseline_promised_is_refused
run mf_encoder::tests::a_frame_is_a_keyframe_only_when_the_sample_says_it_is_a_clean_point
run mf_encoder::tests::an_open_encoder_takes_frames_and_holds_the_asks
run mf_encoder::tests::a_driver_that_goes_silent_is_called_hung_after_two_seconds_of_it
run mf_encoder::tests::a_closed_encoder_ends_its_stream
run mf_encoder::tests::the_probe_answers_on_a_thread_of_its_own
run mf_encoder::tests::nv12_is_the_y_plane_and_the_two_chroma_planes_interleaved
run mf_encoder::tests::nv12_of_a_short_buffer_writes_nothing_past_it
run mf_encoder::tests::annex_b_already_annex_b_is_untouched
run mf_encoder::tests::length_prefixed_units_come_out_behind_start_codes
run mf_encoder::tests::a_truncated_or_empty_stream_never_runs_past_itself
run mf_encoder::tests::sizes_and_rates_are_packed_width_over_height_and_thousandths
run picture::desktop::tests::a_refused_hardware_encoder_settles_on_the_software_one
printf '%s\n' 'PASS: the hardware encoder is chosen when present, refuses a wire it was not promised, and OpenH264 stays the fallback.'
