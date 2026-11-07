#![no_main]
//! A pasted link code and a link channel message: parse, never panic.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    e2ee::parsers::link(data);
});
