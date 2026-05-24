//! Bit-level utility functions for reading/writing packed binary data.
//!
//! All functions treat byte slices as little-endian bit arrays:
//! bit 0 is the LSB of byte 0, bit 8 is the LSB of byte 1, etc.

/// Read up to 64 bits from a byte slice at an arbitrary bit offset.
///
/// When `shift + bit_count > 64` (unaligned 64-bit read), a 9th byte is
/// needed beyond what fits in a u64 buffer. This is handled by reading
/// the extra byte separately and merging its bits into the result.
pub fn read_u64(src: &[u8], bit_offset: usize, bit_count: usize) -> u64 {
    debug_assert!(bit_count <= 64);
    if bit_count == 0 {
        return 0;
    }
    let start_byte = bit_offset / 8;
    let shift = bit_offset % 8;
    let bytes_needed = (shift + bit_count).div_ceil(8);
    let mut buf = 0u64;
    let count = bytes_needed.min(8);
    let mut i = 0;
    while i < count {
        buf |= (src[start_byte + i] as u64) << (i * 8);
        i += 1;
    }
    let mut result = buf >> shift;
    // When shift > 0 and bit_count is large enough, we need the 9th byte
    if bytes_needed > 8 {
        let extra = src[start_byte + 8] as u64;
        result |= extra << (64 - shift);
    }
    let mask = if bit_count >= 64 {
        !0u64
    } else {
        (1u64 << bit_count) - 1
    };
    result & mask
}

/// Write up to 64 bits to a byte slice at an arbitrary bit offset.
///
/// Handles the 9th byte case when `shift + bit_count > 64`.
pub fn write_u64(dst: &mut [u8], bit_offset: usize, bit_count: usize, val: u64) {
    debug_assert!(bit_count <= 64);
    if bit_count == 0 {
        return;
    }
    let start_byte = bit_offset / 8;
    let shift = bit_offset % 8;
    let bytes_needed = (shift + bit_count).div_ceil(8);
    let mask = if bit_count >= 64 {
        !0u64
    } else {
        (1u64 << bit_count) - 1
    };
    let val = val & mask;

    if bytes_needed <= 8 {
        // Fast path: everything fits in 8 bytes
        let mut buf = 0u64;
        let mut i = 0;
        while i < bytes_needed {
            buf |= (dst[start_byte + i] as u64) << (i * 8);
            i += 1;
        }
        let shifted_mask = if shift + bit_count >= 64 {
            !((1u64 << shift) - 1)
        } else {
            mask << shift
        };
        buf = (buf & !shifted_mask) | (val << shift);
        i = 0;
        while i < bytes_needed {
            dst[start_byte + i] = (buf >> (i * 8)) as u8;
            i += 1;
        }
    } else {
        // Slow path: need 9 bytes. Write low 8 bytes, then patch the 9th.
        let mut buf = 0u64;
        let mut i = 0;
        while i < 8 {
            buf |= (dst[start_byte + i] as u64) << (i * 8);
            i += 1;
        }
        // Clear bits [shift..64) in buf and set them from val
        let low_mask = !((1u64 << shift) - 1);
        buf = (buf & !low_mask) | (val << shift);
        i = 0;
        while i < 8 {
            dst[start_byte + i] = (buf >> (i * 8)) as u8;
            i += 1;
        }
        // Handle the 9th byte: bits that spilled past the 8-byte boundary
        let extra_bits = shift + bit_count - 64;
        let extra_val = (val >> (64 - shift)) as u8;
        let extra_mask = (1u8 << extra_bits) - 1;
        dst[start_byte + 8] = (dst[start_byte + 8] & !extra_mask) | (extra_val & extra_mask);
    }
}

/// Check if `bit_count` bits starting at `bit_offset` are all zero.
pub fn are_zero(src: &[u8], bit_offset: usize, bit_count: usize) -> bool {
    if bit_count == 0 {
        return true;
    }
    let mut offset = bit_offset;
    let mut remaining = bit_count;
    while remaining > 64 {
        if read_u64(src, offset, 64) != 0 {
            return false;
        }
        offset += 64;
        remaining -= 64;
    }
    read_u64(src, offset, remaining) == 0
}

/// Zero `bit_count` bits in `dst` starting at `bit_offset`.
pub fn zero(dst: &mut [u8], bit_offset: usize, bit_count: usize) {
    let mut offset = bit_offset;
    let mut remaining = bit_count;
    while remaining > 64 {
        write_u64(dst, offset, 64, 0);
        offset += 64;
        remaining -= 64;
    }
    if remaining > 0 {
        write_u64(dst, offset, remaining, 0);
    }
}

/// Copy `bit_count` bits from `src` at `src_bit` to `dst` at `dst_bit`.
pub fn copy(src: &[u8], src_bit: usize, dst: &mut [u8], dst_bit: usize, bit_count: usize) {
    if bit_count == 0 {
        return;
    }
    let mut s = src_bit;
    let mut d = dst_bit;
    let mut remaining = bit_count;
    while remaining > 64 {
        let val = read_u64(src, s, 64);
        write_u64(dst, d, 64, val);
        s += 64;
        d += 64;
        remaining -= 64;
    }
    if remaining > 0 {
        let val = read_u64(src, s, remaining);
        write_u64(dst, d, remaining, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_write_u64_aligned() {
        let mut buf = [0u8; 8];
        write_u64(&mut buf, 0, 8, 0xAB);
        assert_eq!(read_u64(&buf, 0, 8), 0xAB);
        assert_eq!(buf[0], 0xAB);
    }

    #[test]
    fn test_read_write_u64_unaligned() {
        let mut buf = [0u8; 4];
        // Write 7 bits at offset 3
        write_u64(&mut buf, 3, 7, 0x5A);
        assert_eq!(read_u64(&buf, 3, 7), 0x5A);
        // Original bits should be preserved
        assert_eq!(read_u64(&buf, 0, 3), 0);
    }

    #[test]
    fn test_read_write_bool() {
        let mut buf = [0u8; 1];
        write_u64(&mut buf, 3, 1, 1);
        assert_eq!(read_u64(&buf, 3, 1), 1);
        assert_eq!(read_u64(&buf, 2, 1), 0);
        assert_eq!(buf[0], 0x08);
    }

    #[test]
    fn test_are_zero() {
        let buf = [0u8; 8];
        assert!(are_zero(&buf, 0, 64));

        let mut buf2 = [0u8; 8];
        buf2[3] = 1;
        assert!(!are_zero(&buf2, 0, 64));
        assert!(are_zero(&buf2, 0, 24));
    }

    #[test]
    fn test_copy_aligned() {
        let src = [0xAB, 0xCD, 0xEF, 0x12];
        let mut dst = [0u8; 4];
        copy(&src, 0, &mut dst, 0, 32);
        assert_eq!(dst, src);
    }

    #[test]
    fn test_copy_unaligned() {
        let src = [0xFF, 0x00];
        let mut dst = [0u8; 2];
        // Copy 4 bits from src offset 2 to dst offset 4
        copy(&src, 2, &mut dst, 4, 4);
        // src bits 2..6 = 0b1111 (from 0xFF)
        // Written to dst bits 4..8
        assert_eq!(read_u64(&dst, 4, 4), 0xF);
    }

    #[test]
    fn test_copy_wide() {
        // Test copying >64 bits
        let src = [0xAA; 16];
        let mut dst = [0u8; 16];
        copy(&src, 0, &mut dst, 0, 128);
        assert_eq!(dst, src);
    }

    #[test]
    fn test_read_write_u64_unaligned_full() {
        // Test the 9th byte edge case: 64 bits at non-byte-aligned offset
        let mut buf = [0u8; 10];
        let val: u64 = 0xDEAD_BEEF_CAFE_BABE;
        write_u64(&mut buf, 3, 64, val);
        assert_eq!(read_u64(&buf, 3, 64), val);
        // Verify surrounding bits are preserved
        assert_eq!(read_u64(&buf, 0, 3), 0);
    }

    #[test]
    fn test_copy_unaligned_wide() {
        // Test copying >64 bits at non-byte-aligned offset (triggers 9th byte path)
        // Simulate: 10 bytes at bit offset 19 (byte 2 bit 3) within a container
        let expected = *b"TestPkmn\0\0";
        let mut container = [0u8; 20];
        copy(&expected, 0, &mut container, 19, 80);
        let mut result = [0u8; 10];
        copy(&container, 19, &mut result, 0, 80);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_zero() {
        let mut buf = [0xFFu8; 8];
        zero(&mut buf, 4, 16);
        assert_eq!(read_u64(&buf, 4, 16), 0);
        // Surrounding bits preserved
        assert_eq!(read_u64(&buf, 0, 4), 0xF);
        assert_eq!(read_u64(&buf, 20, 4), 0xF);
    }

    #[test]
    fn test_zero_wide() {
        let mut buf = [0xFFu8; 20];
        zero(&mut buf, 8, 128);
        assert!(are_zero(&buf, 8, 128));
        assert_eq!(buf[0], 0xFF); // preserved
        assert_eq!(buf[17], 0xFF); // preserved
    }

    #[test]
    fn test_20bit_moveslot_packing() {
        // Simulate MoveSlot: 4 flag bits + 9-bit ID + 7-bit PP = 20 bits
        let mut data = [0u8; 40]; // enough for 4 moves at 20-bit stride

        // Move 0 at offset 0: flags=0b0101, id=100, pp=25
        let move0 = 0b0101u64 | (100u64 << 4) | (25u64 << 13);
        write_u64(&mut data, 0, 20, move0);

        // Move 1 at offset 20
        let move1 = 0b0011u64 | (200u64 << 4) | (30u64 << 13);
        write_u64(&mut data, 20, 20, move1);

        // Read back
        assert_eq!(read_u64(&data, 0, 4), 0b0101); // flags
        assert_eq!(read_u64(&data, 4, 9), 100); // id
        assert_eq!(read_u64(&data, 13, 7), 25); // pp

        assert_eq!(read_u64(&data, 20, 4), 0b0011);
        assert_eq!(read_u64(&data, 24, 9), 200);
        assert_eq!(read_u64(&data, 33, 7), 30);
    }
}
