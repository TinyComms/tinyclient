#![no_main]
//! The 13 byte header of a sealed attachment.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    e2ee::parsers::blob_header(data);
});
