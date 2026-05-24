use core::mem;

use crate::bits;
use crate::data_type::*;

pub use zorua_macro::*;

//------------- Field trait + impls -------------

/// const assertion that ensures that target has a known
/// endianness, so that the [ZoruaStruct] trait is well-formed.
const _: () = assert!(
    cfg!(target_endian = "big") || cfg!(target_endian = "little"),
    "This crate can only be compiled on little or big endian systems"
);

/// Compile-time assertion that `A` can be safely transmuted to `B`.
///
/// Checks:
/// - Sizes are equal
/// - Alignment of `A` is >= alignment of `B`
const fn assert_transmute_compatible<A, B>() {
    assert!(
        mem::size_of::<A>() == mem::size_of::<B>(),
        "transmute: size mismatch"
    );
    assert!(
        mem::align_of::<A>() >= mem::align_of::<B>(),
        "transmute: alignment mismatch"
    );
}

/// # Safety
/// This trait is safe to implement *iff*:
/// - `Self` a *POD*, which is to say any possible bit pattern produces a valid instance of it.
/// - The representation of `Self` is independent of the endianness of the system.
pub unsafe trait ZoruaStruct: Sized {
    fn as_bytes(&self) -> &[u8] {
        let slf: *const Self = self;
        unsafe { core::slice::from_raw_parts(slf.cast::<u8>(), mem::size_of::<Self>()) }
    }

    fn as_bytes_mut(&mut self) -> &mut [u8] {
        let slf: *mut Self = self;
        unsafe { core::slice::from_raw_parts_mut(slf.cast::<u8>(), mem::size_of::<Self>()) }
    }

    /// Transmutes a [`ZoruaStruct`] into another with a compatible layout.
    fn transmute<T: ZoruaStruct>(self) -> T {
        const { assert_transmute_compatible::<Self, T>() };
        // SAFETY: Layout compatibility verified by const assertion above
        unsafe {
            let result = core::ptr::read(&self as *const Self as *const T);
            core::mem::forget(self);
            result
        }
    }

    #[cfg(feature = "std")]
    fn box_transmute<T: ZoruaStruct>(self: Box<Self>) -> Box<T> {
        const { assert_transmute_compatible::<Self, T>() };
        unsafe { mem::transmute(self) }
    }

    fn transmute_ref<T: ZoruaStruct>(&self) -> &T {
        const { assert_transmute_compatible::<Self, T>() };
        unsafe { mem::transmute(self) }
    }

    fn transmute_mut<T: ZoruaStruct>(&mut self) -> &mut T {
        const { assert_transmute_compatible::<Self, T>() };
        unsafe { mem::transmute(self) }
    }

    fn transmute_split_ref<A: ZoruaStruct, B: ZoruaStruct>(&self) -> (&A, &B) {
        const { assert_transmute_compatible::<Self, (A, B)>() };
        unsafe {
            let base = self as *const Self as *const u8;
            let a_ptr = base as *const A;
            let b_ptr = base.add(mem::size_of::<A>()) as *const B;
            (&*a_ptr, &*b_ptr)
        }
    }

    fn transmute_split_mut<A: ZoruaStruct, B: ZoruaStruct>(&mut self) -> (&mut A, &mut B) {
        const { assert_transmute_compatible::<Self, (A, B)>() };
        unsafe {
            let base = self as *mut Self as *mut u8;
            let a_ptr = base as *mut A;
            let b_ptr = base.add(mem::size_of::<A>()) as *mut B;
            (&mut *a_ptr, &mut *b_ptr)
        }
    }
}

unsafe impl ZoruaStruct for u8 {}
unsafe impl ZoruaStruct for u16 {}
unsafe impl ZoruaStruct for u32 {}
unsafe impl ZoruaStruct for u64 {}

unsafe impl ZoruaStruct for () {}

unsafe impl<const N: usize, T: ZoruaStruct> ZoruaStruct for [T; N] {}

macro_rules! impl_zorua_struct_for_tuple {
    ($($T:ident),*) => {
        unsafe impl<$($T: ZoruaStruct),*> ZoruaStruct for ($($T,)*) {}
    };
}

impl_zorua_struct_for_tuple!(T, U);
impl_zorua_struct_for_tuple!(T, U, V);
impl_zorua_struct_for_tuple!(T, U, V, W);
impl_zorua_struct_for_tuple!(T, U, V, W, X);
impl_zorua_struct_for_tuple!(T, U, V, W, X, Y);
impl_zorua_struct_for_tuple!(T, U, V, W, X, Y, Z);

// =====================================================================
// Zorua<S> — unified bit read/write trait
// =====================================================================

/// A type that can be read/written as `S`-representation bits.
///
/// `S` selects the storage format (width + endianness).
/// The identity impl `Zorua<Self>` is always the default.
pub trait Zorua<S>: Sized + Clone {
    /// Number of bits in this storage representation.
    const BITS: usize;

    /// Whether reading can fail (e.g., enum with fewer variants than bit patterns).
    const IS_FALLIBLE: bool;

    /// Read from bit-packed bytes at an arbitrary bit offset.
    fn read_bits(src: &[u8], bit_offset: usize) -> Self;

    /// Try to read from bit-packed bytes. Returns Err(raw_value) on invalid data.
    fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, S> {
        Ok(Self::read_bits(src, bit_offset))
    }

    /// Write to bit-packed bytes at an arbitrary bit offset.
    fn write_bits(&self, dst: &mut [u8], bit_offset: usize);
}

// =====================================================================
// Zorua identity impls for bool
// =====================================================================

impl Zorua<bool> for bool {
    const BITS: usize = 1;
    const IS_FALLIBLE: bool = false;

    #[inline]
    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
        bits::read_u64(src, bit_offset, 1) != 0
    }

    #[inline]
    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
        bits::write_u64(dst, bit_offset, 1, *self as u64);
    }
}

// =====================================================================
// Zorua identity impls for u8, u16, u32, u64
// =====================================================================

macro_rules! impl_zorua_identity_for_primitives {
    ($($ty:ident),*) => {
        $(
            impl Zorua<$ty> for $ty {
                const BITS: usize = <$ty>::BITS as usize;
                const IS_FALLIBLE: bool = false;

                #[inline]
                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    bits::read_u64(src, bit_offset, <$ty>::BITS as usize) as $ty
                }

                #[inline]
                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    bits::write_u64(dst, bit_offset, <$ty>::BITS as usize, *self as u64);
                }
            }
        )*
    };
}

impl_zorua_identity_for_primitives!(u8, u16, u32, u64);

// =====================================================================
// Zorua identity impls for ux2 types (u1-u7, u9-u15, u17-u31)
// =====================================================================

macro_rules! impl_zorua_identity_for_ux2 {
    ($($ty:ident),*) => {
        $(
            impl Zorua<$ty> for $ty {
                const BITS: usize = <$ty>::BITS as usize;
                const IS_FALLIBLE: bool = false;

                #[inline]
                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    $ty::new(bits::read_u64(src, bit_offset, <$ty>::BITS as usize) as _)
                }

                #[inline]
                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    let val: u64 = (*self).into();
                    bits::write_u64(dst, bit_offset, <$ty>::BITS as usize, val);
                }
            }
        )*
    };
}

impl_zorua_identity_for_ux2!(u1, u2, u3, u4, u5, u6, u7, u9, u10, u11, u12, u13, u14, u15);
impl_zorua_identity_for_ux2!(
    u17, u18, u19, u20, u21, u22, u23, u24, u25, u26, u27, u28, u29, u30, u31
);

// =====================================================================
// Zorua identity impls for endian types (u16_le, u32_le, etc.)
// =====================================================================

macro_rules! impl_zorua_identity_for_endian {
    ($wrapper:ident, $bits:expr) => {
        impl<E: Endian> Zorua<$wrapper<E>> for $wrapper<E> {
            const BITS: usize = $bits;
            const IS_FALLIBLE: bool = false;

            #[inline]
            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                // Read raw bits and construct via new() — the bytes are already in the
                // correct endian order because read_u64 reads LE and we construct from
                // the native value.
                $wrapper::new(bits::read_u64(src, bit_offset, $bits) as _)
            }

            #[inline]
            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                bits::write_u64(dst, bit_offset, $bits, self.value() as u64);
            }
        }
    };
}

impl_zorua_identity_for_endian!(U16, 16);
impl_zorua_identity_for_endian!(U32, 32);
impl_zorua_identity_for_endian!(U64, 64);

// =====================================================================
// Zorua cross impls: primitives → endian types
// (e.g., u16: Zorua<u16_le> for flat `as u16_le` fields)
// =====================================================================

macro_rules! impl_zorua_cross_for_endian {
    ($native:ty, $wrapper:ident, $bits:expr) => {
        impl<E: Endian> Zorua<$wrapper<E>> for $native {
            const BITS: usize = $bits;
            const IS_FALLIBLE: bool = false;

            #[inline]
            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                bits::read_u64(src, bit_offset, $bits) as $native
            }

            #[inline]
            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                bits::write_u64(dst, bit_offset, $bits, *self as u64);
            }
        }
    };
}

impl_zorua_cross_for_endian!(u16, U16, 16);
impl_zorua_cross_for_endian!(u32, U32, 32);
impl_zorua_cross_for_endian!(u64, U64, 64);

// =====================================================================
// Zorua identity impl for arrays: [T; N] where T: Zorua<T>
// =====================================================================

impl<T: Zorua<T>, const N: usize> Zorua<[T; N]> for [T; N] {
    const BITS: usize = <T as Zorua<T>>::BITS * N;
    const IS_FALLIBLE: bool = false;

    #[inline]
    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
        let stride = <T as Zorua<T>>::BITS;
        core::array::from_fn(|i| T::read_bits(src, bit_offset + i * stride))
    }

    #[inline]
    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
        let stride = <T as Zorua<T>>::BITS;
        for (i, elem) in self.iter().enumerate() {
            elem.write_bits(dst, bit_offset + i * stride);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    bitfields! {
        #[repr(C)]
        #[derive(ZoruaStruct, Clone, Debug, PartialEq, Eq)]
        struct Slot {
            data: u32_le {
                pub id: u9@0,
                pub level: u7@9,
            },
        }
    }

    impl Zorua<Slot> for Slot {
        const BITS: usize = 16;
        const IS_FALLIBLE: bool = false;
        fn read_bits(src: &[u8], bit_offset: usize) -> Self {
            Self {
                data: u32_le::new(bits::read_u64(src, bit_offset, 16) as u32),
            }
        }
        fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
            bits::write_u64(dst, bit_offset, 16, self.data.value() as u64);
        }
    }

    bitfields! {
        #[repr(C)]
        #[derive(ZoruaStruct, Clone, Debug, PartialEq, Eq)]
        struct Container {
            data: [u8; 16] {
                #[zeroedoption]
                pub slots: [Slot; 4]@8,
            },
        }
    }

    #[test]
    fn test_zeroedoption_array_subfield() {
        let mut c = Container { data: [0u8; 16] };

        // All slots start as None
        assert!(c.slots(0).is_none());
        assert!(c.slots(1).is_none());

        // Write a slot
        let mut slot = Slot {
            data: u32_le::new(0),
        };
        slot.set_id(u9::new(42));
        slot.set_level(u7::new(5));
        c.set_slots(0, Some(slot.clone()));

        assert_eq!(c.slots(0).unwrap().id(), u9::new(42));
        assert_eq!(c.slots(0).unwrap().level(), u7::new(5));
        assert!(c.slots(1).is_none());

        // Clear a slot
        c.set_slots(0, None);
        assert!(c.slots(0).is_none());
    }
}
