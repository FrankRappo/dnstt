use anyhow::{bail, Result};
use data_encoding::BASE32_DNSSEC;

use crate::MAX_LABEL_LEN;

/// Encode binary data into DNS-safe subdomain labels.
///
/// Format: `<label1>.<label2>...<labelN>.<session_id>.<domain>`
///
/// Each label is base32-encoded (RFC 4648, lowercase, no padding).
/// Max 63 chars per label, typically 3 labels = ~117 base32 chars = ~73 bytes raw data.
pub fn encode_upstream(data: &[u8], session_id: &str, domain: &str) -> Result<String> {
    if data.is_empty() {
        bail!("cannot encode empty data");
    }

    let encoded = BASE32_DNSSEC.encode(data).to_lowercase();

    // Split into labels of max MAX_LABEL_LEN chars
    let labels: Vec<&str> = encoded
        .as_bytes()
        .chunks(MAX_LABEL_LEN)
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect();

    // Build FQDN: <data-labels>.<session_id>.<domain>
    let mut name = String::with_capacity(encoded.len() + session_id.len() + domain.len() + 10);
    for label in &labels {
        name.push_str(label);
        name.push('.');
    }
    name.push_str(session_id);
    name.push('.');
    name.push_str(domain);

    if name.len() > crate::MAX_NAME_LEN {
        bail!(
            "encoded DNS name too long: {} > {}",
            name.len(),
            crate::MAX_NAME_LEN
        );
    }

    Ok(name)
}

/// Decode data from DNS subdomain labels.
///
/// Extracts the data labels (everything before `<session_id>.<domain>`)
/// and base32-decodes them.
pub fn decode_upstream(name: &str, session_id: &str, domain: &str) -> Result<Vec<u8>> {
    let suffix = format!(".{}.{}", session_id, domain);
    let data_part = name
        .strip_suffix(&suffix)
        .or_else(|| {
            // Try with trailing dot
            let suffix_dot = format!("{}.", suffix);
            name.strip_suffix(&suffix_dot)
        })
        .ok_or_else(|| anyhow::anyhow!("name does not end with {}", suffix))?;

    // Remove dots between labels, keep lowercase (BASE32_DNSSEC uses 0-9a-v)
    let encoded: String = data_part.replace('.', "").to_lowercase();

    if encoded.is_empty() {
        bail!("no data labels in DNS name");
    }

    BASE32_DNSSEC
        .decode(encoded.as_bytes())
        .map_err(|e| anyhow::anyhow!("base32 decode error: {}", e))
}

/// Encode data for DNS TXT record responses (base64).
pub fn encode_downstream(data: &[u8]) -> String {
    use data_encoding::BASE64;
    BASE64.encode(data)
}

/// Decode data from DNS TXT record response.
pub fn decode_downstream(txt: &str) -> Result<Vec<u8>> {
    use data_encoding::BASE64;
    BASE64
        .decode(txt.as_bytes())
        .map_err(|e| anyhow::anyhow!("base64 decode error: {}", e))
}

/// Max raw bytes that can fit in upstream DNS query (3 labels).
pub fn max_upstream_payload(session_id: &str, domain: &str) -> usize {
    // Available chars: MAX_NAME_LEN - len(session_id) - len(domain) - dots
    let overhead = session_id.len() + domain.len() + 3; // 2 dots + safety
    let available_chars = crate::MAX_NAME_LEN.saturating_sub(overhead);
    // Account for dots between labels: every MAX_LABEL_LEN chars needs a dot
    let label_count = available_chars / (MAX_LABEL_LEN + 1);
    let total_encoded_chars = label_count * MAX_LABEL_LEN;
    // base32: 5 bits per char, 8 bits per byte → ratio 5/8
    total_encoded_chars * 5 / 8
}

fn pad_base32(s: &str) -> String {
    let rem = s.len() % 8;
    if rem == 0 {
        s.to_string()
    } else {
        let padding = 8 - rem;
        format!("{}{}", s, "=".repeat(padding))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upstream_roundtrip() {
        let data = b"Hello, DNS tunnel!";
        let session = "abc123";
        let domain = "tunnel.example.com";

        let encoded = encode_upstream(data, session, domain).unwrap();
        assert!(encoded.ends_with(&format!("{}.{}", session, domain)));

        let decoded = decode_upstream(&encoded, session, domain).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_upstream_large_data() {
        // Test with data that requires multiple labels
        let data = vec![0xAB; 100];
        let session = "sess1";
        let domain = "tunnel.example.com";

        let encoded = encode_upstream(&data, session, domain).unwrap();
        let decoded = decode_upstream(&encoded, session, domain).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_downstream_roundtrip() {
        let data = b"Response data with binary \x00\xff\x80";
        let encoded = encode_downstream(data);
        let decoded = decode_downstream(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_max_payload_reasonable() {
        let max = max_upstream_payload("abcd1234", "tunnel.example.com");
        // Should be roughly 100-200 bytes
        assert!(max > 50, "max payload too small: {}", max);
        assert!(max < 300, "max payload too large: {}", max);
    }

    #[test]
    fn test_empty_data_error() {
        assert!(encode_upstream(b"", "s", "d.com").is_err());
    }
}
