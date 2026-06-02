# Zorua

Zorua is a Rust library for modeling binary data with endian-aware storage,
safe byte views, and LSB0 bitfields.

## Byte Layout

Zorua uses [`zerocopy`](https://docs.rs/zerocopy/) as its byte-layout
foundation. File-format storage types should implement the zerocopy byte-view
traits and Zorua's `ByteRepr` blanket trait.

Native multi-byte primitives are not valid zero-copy file storage because their
bytes are host-endian and aligned. Use endian-aware aliases instead:

```rust
use zorua::prelude::*;

let value = u32_be::new(0x12345678);
assert_eq!(value.get(), 0x12345678);
assert_eq!(value.as_bytes(), &[0x12, 0x34, 0x56, 0x78]);
```

## Bit Codecs

`BitCodec<S>` describes how a native value is read from and written to a bit
storage representation `S`.

```rust
use zorua::prelude::*;

#[repr(u8)]
#[derive(BitCodec, Clone, Copy, Debug, PartialEq, Eq)]
enum Language {
    Japanese = 1,
    English = 2,
    French = 3,
}
```

Fallible codecs return the raw storage value from `try_read_bits` when the
stored bits do not map to a valid native value.

## `bitstruct!`

`bitstruct!` defines byte-backed structs and bit-packed fields. It injects the
zerocopy derives needed for safe byte views, rejects native multi-byte storage,
and supports a struct-level default endian for Rust-like field declarations.

```rust
use zorua::prelude::*;

bitstruct! {
    #[repr(C)]
    #[endian(little)]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Header {
        pub id: u16,
        flags: u16 {
            pub kind: u4@0,
            pub active: bool@4,
        },
    }
}
```

Repeated packed fields expose checked views:

```rust
# use zorua::prelude::*;
# bitstruct! {
#   #[repr(C)]
#   #[derive(Clone, Debug, PartialEq, Eq)]
#   pub struct Flags {
#     data: [u8; 4] {
          pub bits: [bool; 32]@0,
#     },
#   }
# }
let mut flags = Flags::new_zeroed();
assert_eq!(flags.bits().len(), 32);
assert_eq!(flags.bits().get(0), Some(false));
assert!(flags.bits_mut().set(0, true));
assert_eq!(flags.bits().get(0), Some(true));
```

## No-Std Support

`zorua` is `no_std` by default. The default feature enables `std`, currently
for compatibility with existing downstream builds.
