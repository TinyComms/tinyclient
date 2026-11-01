#![no_main]
//! A whole sealed attachment, opened with a fixed key: header, chunks, tags.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    e2ee::parsers::sealed_blob(data);
});
