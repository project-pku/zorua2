use core::marker::PhantomData;

use crate::{bits, data_type::*};
use zerocopy::{
    FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned, byteorder::ByteOrder,
};

pub use zorua_macro::*;

/// Byte-level representation for file-format storage types.
///
/// `ByteRepr` is intentionally only a convenience bound over `zerocopy`'s
/// proven byte-view traits. Multi-byte native primitives do not satisfy this
/// bound because they are aligned and host-endian; use endian-aware storage
/// aliases such as [`u16_le`] or [`u32_be`] instead.
pub trait ByteRepr:
    FromBytes + FromZeros + IntoBytes + KnownLayout + Immutable + Unaligned + Sized
{
    #[inline]
    fn read_ref(bytes: &[u8]) -> Option<&Self> {
        Self::ref_from_bytes(bytes).ok()
    }

    #[inline]
    fn read_mut(bytes: &mut [u8]) -> Option<&mut Self> {
        Self::mut_from_bytes(bytes).ok()
    }

    #[inline]
    fn read_prefix(bytes: &[u8]) -> Option<(&Self, &[u8])> {
        Self::ref_from_prefix(bytes).ok()
    }

    #[inline]
    fn read_prefix_mut(bytes: &mut [u8]) -> Option<(&mut Self, &mut [u8])> {
        Self::mut_from_prefix(bytes).ok()
    }

    #[inline]
    fn read_owned(bytes: &[u8]) -> Option<Self>
    where
        Self: Clone,
    {
        Self::read_ref(bytes).cloned()
    }

    #[inline]
    fn view_as<T: ByteRepr>(&self) -> Option<&T> {
        T::read_ref(self.as_bytes())
    }

    #[inline]
    fn view_as_mut<T: ByteRepr>(&mut self) -> Option<&mut T> {
        T::read_mut(self.as_mut_bytes())
    }

    #[inline]
    fn clone_as<T: ByteRepr + Clone>(&self) -> Option<T> {
        self.view_as::<T>().cloned()
    }

    #[inline]
    fn split_ref<A: ByteRepr, B: ByteRepr>(&self) -> Option<(&A, &B)> {
        let bytes = self.as_bytes();
        let (a, b) = bytes.split_at_checked(core::mem::size_of::<A>())?;
        Some((A::read_ref(a)?, B::read_ref(b)?))
    }

    #[inline]
    fn split_mut<A: ByteRepr, B: ByteRepr>(&mut self) -> Option<(&mut A, &mut B)> {
        let bytes = self.as_mut_bytes();
        let (a, b) = bytes.split_at_mut_checked(core::mem::size_of::<A>())?;
        Some((A::read_mut(a)?, B::read_mut(b)?))
    }
}

impl<T> ByteRepr for T where
    T: FromBytes + FromZeros + IntoBytes + KnownLayout + Immutable + Unaligned + Sized
{
}

#[derive(Clone, Copy)]
pub struct BitArrayView<'a, T, S = T> {
    bytes: &'a [u8],
    bit_offset: usize,
    len: usize,
    stride: usize,
    _marker: PhantomData<fn() -> (T, S)>,
}

impl<'a, T, S> BitArrayView<'a, T, S> {
    #[inline]
    pub const fn new(bytes: &'a [u8], bit_offset: usize, len: usize, stride: usize) -> Self {
        Self {
            bytes,
            bit_offset,
            len,
            stride,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<<T as BitCodec<S>>::Read>
    where
        T: BitCodec<S>,
    {
        (index < self.len).then(|| T::read(self.bytes, self.bit_offset + index * self.stride))
    }

    #[inline]
    pub fn try_get(&self, index: usize) -> Option<Result<T, S>>
    where
        T: BitCodec<S>,
    {
        (index < self.len)
            .then(|| T::try_read_bits(self.bytes, self.bit_offset + index * self.stride))
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = <T as BitCodec<S>>::Read> + '_
    where
        T: BitCodec<S>,
    {
        (0..self.len).map(|index| T::read(self.bytes, self.bit_offset + index * self.stride))
    }
}

pub struct BitArrayViewMut<'a, T, S = T> {
    bytes: &'a mut [u8],
    bit_offset: usize,
    len: usize,
    stride: usize,
    _marker: PhantomData<fn() -> (T, S)>,
}

impl<'a, T, S> BitArrayViewMut<'a, T, S> {
    #[inline]
    pub fn new(bytes: &'a mut [u8], bit_offset: usize, len: usize, stride: usize) -> Self {
        Self {
            bytes,
            bit_offset,
            len,
            stride,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<<T as BitCodec<S>>::Read>
    where
        T: BitCodec<S>,
    {
        (index < self.len).then(|| T::read(self.bytes, self.bit_offset + index * self.stride))
    }

    #[inline]
    pub fn try_get(&self, index: usize) -> Option<Result<T, S>>
    where
        T: BitCodec<S>,
    {
        (index < self.len)
            .then(|| T::try_read_bits(self.bytes, self.bit_offset + index * self.stride))
    }

    #[inline]
    pub fn set(&mut self, index: usize, value: T) -> bool
    where
        T: BitCodec<S>,
    {
        if index >= self.len {
            return false;
        }
        value.write_bits(self.bytes, self.bit_offset + index * self.stride);
        true
    }
}

#[derive(Clone, Copy)]
pub struct ZeroedOptionBitArrayView<'a, T, S = T> {
    bytes: &'a [u8],
    bit_offset: usize,
    len: usize,
    stride: usize,
    bits: usize,
    _marker: PhantomData<fn() -> (T, S)>,
}

impl<'a, T, S> ZeroedOptionBitArrayView<'a, T, S> {
    #[inline]
    pub const fn new(
        bytes: &'a [u8],
        bit_offset: usize,
        len: usize,
        stride: usize,
        bits: usize,
    ) -> Self {
        Self {
            bytes,
            bit_offset,
            len,
            stride,
            bits,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<Option<T>>
    where
        T: BitCodec<S>,
    {
        if index >= self.len {
            return None;
        }
        let offset = self.bit_offset + index * self.stride;
        if bits::are_zero(self.bytes, offset, self.bits) {
            Some(None)
        } else {
            Some(Some(T::read_bits(self.bytes, offset)))
        }
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = Option<T>> + '_
    where
        T: BitCodec<S>,
    {
        (0..self.len).map(|index| self.get(index).expect("index is within view length"))
    }
}

pub struct ZeroedOptionBitArrayViewMut<'a, T, S = T> {
    bytes: &'a mut [u8],
    bit_offset: usize,
    len: usize,
    stride: usize,
    bits: usize,
    _marker: PhantomData<fn() -> (T, S)>,
}

impl<'a, T, S> ZeroedOptionBitArrayViewMut<'a, T, S> {
    #[inline]
    pub fn new(
        bytes: &'a mut [u8],
        bit_offset: usize,
        len: usize,
        stride: usize,
        bits: usize,
    ) -> Self {
        Self {
            bytes,
            bit_offset,
            len,
            stride,
            bits,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, index: usize) -> Option<Option<T>>
    where
        T: BitCodec<S>,
    {
        if index >= self.len {
            return None;
        }
        let offset = self.bit_offset + index * self.stride;
        if bits::are_zero(self.bytes, offset, self.bits) {
            Some(None)
        } else {
            Some(Some(T::read_bits(self.bytes, offset)))
        }
    }

    #[inline]
    pub fn set(&mut self, index: usize, value: Option<T>) -> bool
    where
        T: BitCodec<S>,
    {
        if index >= self.len {
            return false;
        }
        let offset = self.bit_offset + index * self.stride;
        match value {
            Some(value) => value.write_bits(self.bytes, offset),
            None => bits::zero(self.bytes, offset, self.bits),
        }
        true
    }
}

/// A type that can be read/written as an `S` bit-level storage representation.
///
/// All bit offsets use the current Zorua LSB0 convention: bit 0 is the least
/// significant bit of byte 0, bit 8 is the least significant bit of byte 1, and
/// so on.
pub trait BitCodec<S>: Sized {
    /// Number of bits in this storage representation.
    const BITS: usize;

    /// Whether reading can fail because not every storage value is valid.
    const IS_FALLIBLE: bool;

    /// Public read result for this storage representation.
    type Read;

    /// Read from bit-packed bytes at an arbitrary bit offset.
    fn read_bits(src: &[u8], bit_offset: usize) -> Self;

    /// Try to read from bit-packed bytes. Returns the raw storage value on
    /// invalid data.
    fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, S> {
        Ok(Self::read_bits(src, bit_offset))
    }

    /// Read using the public API shape for this codec. Infallible codecs return
    /// `Self`; fallible codecs return `Result<Self, S>`.
    fn read(src: &[u8], bit_offset: usize) -> Self::Read;

    /// Write to bit-packed bytes at an arbitrary bit offset.
    fn write_bits(&self, dst: &mut [u8], bit_offset: usize);
}

impl BitCodec<bool> for bool {
    const BITS: usize = 1;
    const IS_FALLIBLE: bool = false;
    type Read = Self;

    #[inline]
    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
        bits::read_u64(src, bit_offset, 1) != 0
    }

    #[inline]
    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
        bits::write_u64(dst, bit_offset, 1, *self as u64);
    }

    #[inline]
    fn read(src: &[u8], bit_offset: usize) -> Self::Read {
        Self::read_bits(src, bit_offset)
    }
}

macro_rules! impl_bitcodec_identity_for_primitives {
    ($($ty:ident),*) => {
        $(
            impl BitCodec<$ty> for $ty {
                const BITS: usize = <$ty>::BITS as usize;
                const IS_FALLIBLE: bool = false;
                type Read = Self;

                #[inline]
                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    bits::read_u64(src, bit_offset, <$ty>::BITS as usize) as $ty
                }

                #[inline]
                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    bits::write_u64(dst, bit_offset, <$ty>::BITS as usize, *self as u64);
                }

                #[inline]
                fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                    <$ty as BitCodec<$ty>>::read_bits(src, bit_offset)
                }
            }
        )*
    };
}

impl_bitcodec_identity_for_primitives!(u8, u16, u32, u64, i8, i16, i32, i64);

macro_rules! impl_bitcodec_identity_for_ux2 {
    ($($ty:ident),*) => {
        $(
            impl BitCodec<$ty> for $ty {
                const BITS: usize = <$ty>::BITS as usize;
                const IS_FALLIBLE: bool = false;
                type Read = Self;

                #[inline]
                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    $ty::new(bits::read_u64(src, bit_offset, <$ty>::BITS as usize) as _)
                }

                #[inline]
                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    let val: u64 = (*self).into();
                    bits::write_u64(dst, bit_offset, <$ty>::BITS as usize, val);
                }

                #[inline]
                fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                    <$ty as BitCodec<$ty>>::read_bits(src, bit_offset)
                }
            }
        )*
    };
}

impl_bitcodec_identity_for_ux2!(u1, u2, u3, u4, u5, u6, u7, u9, u10, u11, u12, u13, u14, u15);
impl_bitcodec_identity_for_ux2!(
    u17, u18, u19, u20, u21, u22, u23, u24, u25, u26, u27, u28, u29, u30, u31
);

macro_rules! impl_unsigned_endian_bitcodec {
    ($wrapper:ident, $native:ty, $bits:expr, $bytes:expr) => {
        impl<O: ByteOrder> BitCodec<$wrapper<O>> for $wrapper<O> {
            const BITS: usize = $bits;
            const IS_FALLIBLE: bool = false;
            type Read = Self;

            #[inline]
            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                let mut bytes = [0u8; $bytes];
                bits::copy(src, bit_offset, &mut bytes, 0, $bits);
                Self::from_bytes(bytes)
            }

            #[inline]
            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                bits::copy(self.as_bytes(), 0, dst, bit_offset, $bits);
            }

            #[inline]
            fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                <$wrapper<O> as BitCodec<$wrapper<O>>>::read_bits(src, bit_offset)
            }
        }

        impl<O: ByteOrder> BitCodec<$wrapper<O>> for $native {
            const BITS: usize = $bits;
            const IS_FALLIBLE: bool = false;
            type Read = Self;

            #[inline]
            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                <$wrapper<O> as BitCodec<$wrapper<O>>>::read_bits(src, bit_offset).get()
            }

            #[inline]
            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                $wrapper::<O>::new(*self).write_bits(dst, bit_offset);
            }

            #[inline]
            fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                <$native as BitCodec<$wrapper<O>>>::read_bits(src, bit_offset)
            }
        }
    };
}

macro_rules! impl_signed_endian_bitcodec {
    ($wrapper:ident, $native:ty, $bits:expr, $bytes:expr) => {
        impl<O: ByteOrder> BitCodec<$wrapper<O>> for $wrapper<O> {
            const BITS: usize = $bits;
            const IS_FALLIBLE: bool = false;
            type Read = Self;

            #[inline]
            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                let mut bytes = [0u8; $bytes];
                bits::copy(src, bit_offset, &mut bytes, 0, $bits);
                Self::from_bytes(bytes)
            }

            #[inline]
            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                bits::copy(self.as_bytes(), 0, dst, bit_offset, $bits);
            }

            #[inline]
            fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                <$wrapper<O> as BitCodec<$wrapper<O>>>::read_bits(src, bit_offset)
            }
        }

        impl<O: ByteOrder> BitCodec<$wrapper<O>> for $native {
            const BITS: usize = $bits;
            const IS_FALLIBLE: bool = false;
            type Read = Self;

            #[inline]
            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                <$wrapper<O> as BitCodec<$wrapper<O>>>::read_bits(src, bit_offset).get()
            }

            #[inline]
            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                $wrapper::<O>::new(*self).write_bits(dst, bit_offset);
            }

            #[inline]
            fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                <$native as BitCodec<$wrapper<O>>>::read_bits(src, bit_offset)
            }
        }
    };
}

impl_unsigned_endian_bitcodec!(U16, u16, 16, 2);
impl_unsigned_endian_bitcodec!(U32, u32, 32, 4);
impl_unsigned_endian_bitcodec!(U64, u64, 64, 8);
impl_signed_endian_bitcodec!(I16, i16, 16, 2);
impl_signed_endian_bitcodec!(I32, i32, 32, 4);
impl_signed_endian_bitcodec!(I64, i64, 64, 8);

impl<T: BitCodec<T>, const N: usize> BitCodec<[T; N]> for [T; N] {
    const BITS: usize = <T as BitCodec<T>>::BITS * N;
    const IS_FALLIBLE: bool = <T as BitCodec<T>>::IS_FALLIBLE;
    type Read = Self;

    #[inline]
    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
        let stride = <T as BitCodec<T>>::BITS;
        core::array::from_fn(|i| T::read_bits(src, bit_offset + i * stride))
    }

    #[inline]
    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
        let stride = <T as BitCodec<T>>::BITS;
        for (i, elem) in self.iter().enumerate() {
            elem.write_bits(dst, bit_offset + i * stride);
        }
    }

    #[inline]
    fn read(src: &[u8], bit_offset: usize) -> Self::Read {
        <Self as BitCodec<[T; N]>>::read_bits(src, bit_offset)
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    bitstruct! {
        #[repr(C)]
        #[endian(little)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        struct Slot {
            bits data: u16 {
                pub id: u9,
                pub level: u7,
            },
        }
    }

    impl BitCodec<Slot> for Slot {
        const BITS: usize = 16;
        const IS_FALLIBLE: bool = false;
        type Read = Self;

        fn read_bits(src: &[u8], bit_offset: usize) -> Self {
            let mut slot = Slot::new_zeroed();
            bits::copy(src, bit_offset, slot.as_mut_bytes(), 0, Self::BITS);
            slot
        }

        fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
            bits::copy(self.as_bytes(), 0, dst, bit_offset, Self::BITS);
        }

        fn read(src: &[u8], bit_offset: usize) -> Self::Read {
            Self::read_bits(src, bit_offset)
        }
    }

    bitstruct! {
        #[repr(C)]
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct Container {
            bits data: [u8; 16] {
                pad 8,
                #[zeroedoption]
                pub slots: [Slot; 4]@8,
                pad 56,
            },
        }
    }

    #[test]
    fn test_zeroedoption_array_subfield() {
        let mut c = Container { data: [0u8; 16] };

        assert!(c.slots().get(0).unwrap().is_none());
        assert!(c.slots().get(1).unwrap().is_none());

        let mut slot = Slot::new_zeroed();
        slot.set_id(u9::new(42));
        slot.set_level(u7::new(5));
        assert!(c.slots_mut().set(0, Some(slot)));

        let slot = c.slots().get(0).unwrap().unwrap();
        assert_eq!(slot.id(), u9::new(42));
        assert_eq!(slot.level(), u7::new(5));
        assert!(c.slots().get(1).unwrap().is_none());

        assert!(c.slots_mut().set(0, None));
        assert!(c.slots().get(0).unwrap().is_none());
    }

    #[test]
    fn test_big_endian_bitcodec_uses_storage_bytes() {
        let value = u16_be::new(0x1234);
        let mut bytes = [0u8; 2];
        value.write_bits(&mut bytes, 0);
        assert_eq!(bytes, [0x12, 0x34]);
        assert_eq!(<u16 as BitCodec<u16>>::read_bits(&bytes, 0), 0x3412);
        assert_eq!(<u16 as BitCodec<u16_be>>::read_bits(&bytes, 0), 0x1234);
    }

    bitstruct! {
        #[repr(C)]
        #[endian(big)]
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct DefaultEndianHeader {
            pub id: u16,
            pub count: u32,
        }
    }

    #[test]
    fn test_default_endian_storage() {
        let mut header = DefaultEndianHeader::new_zeroed();
        header.set_id(0x1234);
        header.set_count(0x01020304);

        assert_eq!(header.as_bytes(), &[0x12, 0x34, 0x01, 0x02, 0x03, 0x04]);
        assert_eq!(header.id(), 0x1234);
        assert_eq!(header.count(), 0x01020304);
    }
}
