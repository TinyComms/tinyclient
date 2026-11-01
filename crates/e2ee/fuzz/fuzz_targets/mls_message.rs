#![no_main]
//! Every MLS shape the engine takes from the server (proposals, commit, Welcome, application
//! ciphertext, external sender key) into a live group. No panic, whatever the bytes.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    e2ee::parsers::mls_message(data);
});
