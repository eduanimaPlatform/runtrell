// SPDX-License-Identifier: Apache-2.0
//! Bounded still-image container inspection, not entropy/pixel decoding.
use super::{Result, invalid};

pub(super) fn jpeg(bytes: &[u8]) -> Result<(u32, u32)> {
    let bad = || invalid("IMAGE_FORMAT");
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return Err(bad());
    }
    let mut offset = 2;
    let mut dimensions = None;
    let mut scanned = false;
    while offset < bytes.len() {
        if bytes[offset] != 0xff {
            return Err(bad());
        }
        while bytes.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *bytes.get(offset).ok_or_else(bad)?;
        offset += 1;
        if marker == 0xd9 {
            return if scanned && offset == bytes.len() {
                dimensions.ok_or_else(bad)
            } else {
                Err(bad())
            };
        }
        if matches!(marker, 0x00 | 0x01 | 0xd0..=0xd8) {
            return Err(bad());
        }
        let length = bytes.get(offset..offset + 2).ok_or_else(bad)?;
        let length = u16::from_be_bytes([length[0], length[1]]) as usize;
        if length < 2 {
            return Err(bad());
        }
        let payload = bytes.get(offset + 2..offset + length).ok_or_else(bad)?;
        offset += length;
        match marker {
            0xc0..=0xc2 => {
                if dimensions.is_some()
                    || payload.len() < 6
                    || !matches!(payload[0], 8 | 12)
                    || payload[5] == 0
                    || payload.len() != 6 + 3 * usize::from(payload[5])
                {
                    return Err(bad());
                }
                dimensions = Some((
                    u32::from(u16::from_be_bytes([payload[3], payload[4]])),
                    u32::from(u16::from_be_bytes([payload[1], payload[2]])),
                ));
            }
            0xda => {
                if dimensions.is_none()
                    || payload.is_empty()
                    || payload[0] == 0
                    || payload.len() != 4 + 2 * usize::from(payload[0])
                {
                    return Err(bad());
                }
                let start = offset;
                // Entropy bytes are opaque. Stuffed FF and restart markers stay
                // inside the scan; subsequent scans/EOI return to the marker loop.
                while offset < bytes.len() {
                    if bytes[offset] != 0xff {
                        offset += 1;
                        continue;
                    }
                    let mut next = offset + 1;
                    while bytes.get(next) == Some(&0xff) {
                        next += 1;
                    }
                    match bytes.get(next) {
                        Some(0x00 | 0xd0..=0xd7) => offset = next + 1,
                        Some(_) => break,
                        None => return Err(bad()),
                    }
                }
                if offset == start {
                    return Err(bad());
                }
                scanned = true;
            }
            // Tables, restart interval, application metadata and comments.
            0xc4 | 0xcc | 0xdb | 0xdd | 0xe0..=0xef | 0xfe => (),
            _ => return Err(bad()),
        }
    }
    Err(bad())
}

pub(super) fn webp(bytes: &[u8]) -> Result<(u32, u32)> {
    let bad = || invalid("IMAGE_FORMAT");
    if bytes.len() < 20
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WEBP"
        || u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())) + 8 != bytes.len() as u64
    {
        return Err(bad());
    }
    let mut offset = 12;
    let mut canvas = None;
    let mut dimensions = None;
    let mut alpha = false;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 8).ok_or_else(bad)?;
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(8)
            .and_then(|n| n.checked_add(size))
            .ok_or_else(bad)?;
        let payload = bytes.get(offset + 8..end).ok_or_else(bad)?;
        let next = end.checked_add(size % 2).ok_or_else(bad)?;
        if next > bytes.len() || (next != end && bytes[end] != 0) {
            return Err(bad());
        }
        match &header[..4] {
            b"VP8X" => {
                if offset != 12
                    || size != 10
                    || payload[0] & 0xc3 != 0
                    || payload[1..4] != [0, 0, 0]
                {
                    return Err(bad());
                }
                let little24 =
                    |b: &[u8]| u32::from(b[0]) | u32::from(b[1]) << 8 | u32::from(b[2]) << 16;
                canvas = Some((little24(&payload[4..7]) + 1, little24(&payload[7..10]) + 1));
            }
            b"VP8 " => {
                if dimensions.is_some()
                    || size <= 10
                    || payload[0] & 1 != 0
                    || &payload[3..6] != b"\x9d\x01\x2a"
                {
                    return Err(bad());
                }
                dimensions = Some((
                    u32::from(u16::from_le_bytes([payload[6], payload[7]]) & 0x3fff),
                    u32::from(u16::from_le_bytes([payload[8], payload[9]]) & 0x3fff),
                ));
            }
            b"VP8L" => {
                if dimensions.is_some()
                    || alpha
                    || size <= 5
                    || payload[0] != 0x2f
                    || payload[4] & 0xe0 != 0
                {
                    return Err(bad());
                }
                let bits = u32::from_le_bytes(payload[1..5].try_into().unwrap());
                dimensions = Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
            }
            b"ALPH" => {
                if canvas.is_none() || dimensions.is_some() || alpha || size <= 1 {
                    return Err(bad());
                }
                alpha = true;
            }
            b"ANIM" | b"ANMF" => return Err(invalid("IMAGE_ANIMATION_UNSUPPORTED")),
            _ => (),
        }
        offset = next;
    }
    let dimensions = dimensions.ok_or_else(bad)?;
    if canvas.is_some_and(|c| c != dimensions) {
        return Err(bad());
    }
    Ok(dimensions)
}
