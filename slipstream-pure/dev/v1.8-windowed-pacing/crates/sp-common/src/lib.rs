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
/// Custom EDNS0 option code for tunnel data (private range)
pub const EDNS0_TUNNEL_OPTION: u16 = 65001;
/// Max upstream data in EDNS0 option (bytes)
pub const EDNS0_MAX_UPSTREAM: usize = 1200;
