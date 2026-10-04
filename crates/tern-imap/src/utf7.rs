//! Modified UTF-7 for mailbox names (RFC 3501 §5.1.3).
//!
//! The local store and the UI use decoded UTF-8 names; only the wire uses
//! this encoding.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

/// Encode a UTF-8 mailbox name.
pub fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        out.push('&');
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            let chars = chunk.len() + 1;
            for i in 0..chars {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out.push('-');
        pending.clear();
    };
    for c in name.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Decode a modified UTF-7 mailbox name. Invalid input is returned as-is
/// (some servers send raw UTF-8).
pub fn decode(name: &str) -> String {
    try_decode(name).unwrap_or_else(|| name.to_owned())
}

fn try_decode(name: &str) -> Option<String> {
    let mut out = String::with_capacity(name.len());
    let mut it = name.split('&');
    out.push_str(it.next()?);
    for part in it {
        let (encoded, rest) = part.split_once('-')?;
        if encoded.is_empty() {
            out.push('&');
        } else {
            let mut bits = 0u32;
            let mut nbits = 0;
            let mut bytes = Vec::new();
            for b in encoded.bytes() {
                let v = B64.iter().position(|&x| x == b)? as u32;
                bits = (bits << 6) | v;
                nbits += 6;
                if nbits >= 8 {
                    nbits -= 8;
                    bytes.push((bits >> nbits) as u8);
                    bits &= (1 << nbits) - 1;
                }
            }
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            out.push_str(&String::from_utf16(&units).ok()?);
        }
        out.push_str(rest);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for (utf8, wire) in [
            ("INBOX", "INBOX"),
            ("Entwürfe", "Entw&APw-rfe"),
            ("Tom & Jerry", "Tom &- Jerry"),
            ("日本語", "&ZeVnLIqe-"),
            ("~peter/mail/台北/日本語", "~peter/mail/&U,BTFw-/&ZeVnLIqe-"),
            ("😀", "&2D3eAA-"),
        ] {
            assert_eq!(encode(utf8), wire, "encode {utf8}");
            assert_eq!(decode(wire), utf8, "decode {wire}");
        }
        assert_eq!(decode("broken&"), "broken&");
    }
}
