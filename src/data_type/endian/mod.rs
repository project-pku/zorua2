//! Endian-aware integer aliases backed by `zerocopy::byteorder`.
//!
//! These types have alignment 1 and store their bytes in the endianness encoded
//! in their type parameter. Prefer these aliases for file-format storage instead
//! of native multi-byte primitives.

mod aliases;

pub use aliases::*;
pub use zerocopy::byteorder::{
    BigEndian as Big, ByteOrder, I16, I32, I64, LittleEndian as Little, U16, U32, U64,
};

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::IntoBytes;

    #[test]
    fn test_u32_endian() {
        let big_endian_value = u32_be::new(0x12345678);
        assert_eq!(big_endian_value.get(), 0x12345678);

        let little_endian_value = u32_le::new(0x12345678);
        assert_eq!(little_endian_value.get(), 0x12345678);
    }

    #[test]
    fn test_endian_behavior() {
        let big_val = u16_be::new(0x1234);
        let little_val = u16_le::new(0x1234);

        assert_eq!(big_val.get(), 0x1234);
        assert_eq!(little_val.get(), 0x1234);
        assert_eq!(big_val.as_bytes(), &[0x12, 0x34]);
        assert_eq!(little_val.as_bytes(), &[0x34, 0x12]);
    }

    #[test]
    fn test_basic_integer_types() {
        let u16_val = u16_le::new(65535);
        assert_eq!(u16_val.get(), 65535);

        let u64_val = u64_be::new(0xFFFFFFFFFFFFFFFF);
        assert_eq!(u64_val.get(), 0xFFFFFFFFFFFFFFFF);

        let i16_val = i16_be::new(-32768);
        assert_eq!(i16_val.get(), -32768);

        let i32_val = i32_le::new(-2147483648);
        assert_eq!(i32_val.get(), -2147483648);

        let i64_val = i64_be::new(-9223372036854775808);
        assert_eq!(i64_val.get(), -9223372036854775808);
    }
}
