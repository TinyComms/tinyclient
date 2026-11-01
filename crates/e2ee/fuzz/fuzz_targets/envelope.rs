#![no_main]
//! The JSON envelope decoder, both kinds. Any bytes: it must return.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    e2ee::parsers::envelope(data);
});
