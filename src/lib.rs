#![cfg_attr(not(feature = "std"), no_std)]

pub mod bits;
pub mod data_type;
pub mod traits;

pub use zerocopy;

#[macro_use]
pub mod prelude {
    pub use crate::bits;
    pub use crate::data_type::*;
    pub use crate::traits::*;
    pub use paste::paste;
    pub use zerocopy::{
        FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned, byteorder::ByteOrder,
    };

    pub use zorua_macro::BitCodec;
    pub use zorua_macro::bitstruct;
}
