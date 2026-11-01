//! The TLS backend for every HTTPS and secure WebSocket connection chatd makes.
//!
//! Desktop and Apple use the system's (`native-tls`: Security.framework, the platform store).
//! Android links no OpenSSL here and has no store this crate reads, so it uses rustls with the
//! bundled roots (see Cargo.toml). Call sites ask this module for the backend, so the choice is
//! made once.

use std::net::TcpStream;
use tungstenite::stream::MaybeTlsStream;
use ureq::tls::{TlsConfig, TlsProvider};

#[cfg(not(target_os = "android"))]
const PROVIDER: TlsProvider = TlsProvider::NativeTls;
#[cfg(target_os = "android")]
const PROVIDER: TlsProvider = TlsProvider::Rustls;

/// The `ureq` TLS settings for a request.
pub fn config() -> TlsConfig {
    TlsConfig::builder().provider(PROVIDER).build()
}

/// The TCP stream under a WebSocket's TLS layer, or the stream itself when it is plain. For the
/// socket options (timeouts, non-blocking reads) that tungstenite does not expose.
pub fn tcp(stream: &MaybeTlsStream<TcpStream>) -> Option<&TcpStream> {
    match stream {
        MaybeTlsStream::Plain(tcp) => Some(tcp),
        #[cfg(not(target_os = "android"))]
        MaybeTlsStream::NativeTls(tls) => Some(tls.get_ref()),
        #[cfg(target_os = "android")]
        MaybeTlsStream::Rustls(tls) => Some(tls.get_ref()),
        _ => None,
    }
}
