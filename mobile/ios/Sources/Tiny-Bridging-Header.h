//
//  Tiny-Bridging-Header.h
//
//  The only Rust symbol the Swift shell names. It is exported by the static library
//  (libtinyclient.a inside TinyCore.xcframework) from
//  crates/tinyclient/src/lib.rs: `#[no_mangle] pub extern "C" fn tinyclient_ios_main()`.
//
//  A bridging header is enough for one function. When the surface grows past this — events,
//  requests, async — that is the point to bring in UniFFI, not now
//  (research_notes/mobile/rust-core-native-shells.md §1).
//
#ifndef TINY_BRIDGING_HEADER_H
#define TINY_BRIDGING_HEADER_H

/// Starts the Rust window. Call once, on the main thread.
void tinyclient_ios_main(void);

#endif /* TINY_BRIDGING_HEADER_H */
