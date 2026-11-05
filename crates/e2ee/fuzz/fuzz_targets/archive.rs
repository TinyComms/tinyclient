#![no_main]
//! A whole history archive, opened with a fixed key: header, chunks, records.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    e2ee::parsers::archive(data);
});
