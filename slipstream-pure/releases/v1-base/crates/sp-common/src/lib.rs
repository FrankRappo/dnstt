pub mod config;
pub mod dns_codec;
pub mod compress;
pub mod crypto;
pub mod protocol;

/// Maximum DNS label length (RFC 1035)
pub const MAX_LABEL_LEN: usize = 63;
/// Maximum DNS name length
pub const MAX_NAME_LEN: usize = 253;
/// Domain suffix for tunnel queries
pub const DEFAULT_DOMAIN: &str = "tunnel.example.com";
/// Default SOCKS5 client listen port
pub const DEFAULT_CLIENT_PORT: u16 = 7000;
/// Default SOCKS5 server forward port
pub const DEFAULT_SERVER_SOCKS_PORT: u16 = 1080;
/// EDNS0 max UDP payload
pub const EDNS0_PAYLOAD_SIZE: u16 = 4096;
