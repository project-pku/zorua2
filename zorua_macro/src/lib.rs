use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Expr, Ident, LitInt, Path, Token, Type,
    Visibility, braced,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    token,
};

/// Formats a prefixed/suffixed identifier, moving leading underscores before the prefix.
/// e.g. `prefixed_name("set_", "_unused", "")` → `"_set_unused"`.
fn prefixed_name(prefix: &str, name: &syn::Ident, suffix: &str) -> syn::Ident {
    let name_str = name.to_string();
    let trimmed = name_str.trim_start_matches('_');
    let underscores = &name_str[..name_str.len() - trimmed.len()];
    syn::Ident::new(
        &format!("{underscores}{prefix}{trimmed}{suffix}"),
        name.span(),
    )
}

fn has_derive(attrs: &[Attribute], derive_name: &str) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }

        attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
            .is_ok_and(|paths| {
                paths.iter().any(|path| {
                    path.segments
                        .last()
                        .is_some_and(|segment| segment.ident == derive_name)
                })
            })
    })
}

#[derive(Clone, Copy)]
enum DefaultEndian {
    Little,
    Big,
}

fn extract_default_endian(attrs: &mut Vec<Attribute>) -> Result<Option<DefaultEndian>, syn::Error> {
    let mut default = None;
    let mut retained = Vec::new();

    for attr in attrs.drain(..) {
        if !attr.path().is_ident("endian") {
            retained.push(attr);
            continue;
        }

        let ident: Ident = attr.parse_args()?;
        let parsed = match ident.to_string().as_str() {
            "little" => DefaultEndian::Little,
            "big" => DefaultEndian::Big,
            _ => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "expected `little` or `big` in #[endian(...)]",
                ));
            }
        };
        if default.replace(parsed).is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "duplicate #[endian(...)] attribute",
            ));
        }
    }

    *attrs = retained;
    Ok(default)
}

fn native_multibyte_storage(ty: &Type, endian: DefaultEndian) -> Option<Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    if path.qself.is_some() || path.path.segments.len() != 1 {
        return None;
    }

    let suffix = match endian {
        DefaultEndian::Little => "le",
        DefaultEndian::Big => "be",
    };
    let ident = path.path.segments.first()?.ident.to_string();
    let storage = match ident.as_str() {
        "u16" | "u32" | "u64" | "i16" | "i32" | "i64" => format!("{ident}_{suffix}"),
        _ => return None,
    };
    syn::parse_str(&storage).ok()
}

fn endian_storage_type(ty: &Type, endian: DefaultEndian) -> Option<Type> {
    if let Some(storage) = native_multibyte_storage(ty, endian) {
        return Some(storage);
    }

    if let Type::Array(array) = ty {
        let elem = endian_storage_type(&array.elem, endian)?;
        let mut array = array.clone();
        array.elem = Box::new(elem);
        return Some(Type::Array(array));
    }

    None
}

fn is_native_multibyte(ty: &Type) -> bool {
    let Type::Path(path) = ty else {
        return false;
    };
    if path.qself.is_some() || path.path.segments.len() != 1 {
        return false;
    }
    matches!(
        path.path
            .segments
            .first()
            .map(|s| s.ident.to_string())
            .as_deref(),
        Some("u16" | "u32" | "u64" | "i16" | "i32" | "i64")
    )
}

fn contains_native_multibyte(ty: &Type) -> bool {
    if is_native_multibyte(ty) {
        return true;
    }

    if let Type::Array(array) = ty {
        return contains_native_multibyte(&array.elem);
    }

    false
}

// =====================================================================
// derive(BitCodec) — bit read/write trait for enums and newtypes
// =====================================================================

/// Derive macro that implements `BitCodec<S>` trait for enums and newtype structs.
///
/// For enums: generates identity impl + wider storage impls.
/// For newtypes: generates identity impl + delegation impls per known storage type.
#[proc_macro_derive(BitCodec)]
pub fn bitcodec_derive_macro(item: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(item).unwrap();
    let result = match &ast.data {
        Data::Enum(data) => impl_zorua_enum(&ast, data),
        Data::Struct(data) => impl_zorua_struct(&ast, data),
        _ => Err(syn::Error::new_spanned(
            &ast,
            "BitCodec can only be derived for enums and structs",
        )),
    };
    result.unwrap_or_else(|e| e.into_compile_error().into())
}

fn impl_zorua_enum(ast: &DeriveInput, data: &DataEnum) -> Result<TokenStream, syn::Error> {
    let repr_state = get_repr_state(&ast.attrs)?;

    let (cast_repr, max_bits): (Type, usize) = if repr_state.repr_u8 {
        (syn::parse_str("u8").unwrap(), 8)
    } else if repr_state.repr_u16 {
        (syn::parse_str("u16").unwrap(), 16)
    } else if repr_state.repr_u32 {
        (syn::parse_str("u32").unwrap(), 32)
    } else {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "BitCodec requires #[repr(u8)], #[repr(u16)], or #[repr(u32)] for enums",
        ));
    };

    let ident = &ast.ident;
    let (impl_generics, type_generics, where_clause) = ast.generics.split_for_impl();

    // Validate variants and build match arms
    let mut match_arms = quote! {};
    let mut try_match_arms = quote! {};
    let mut write_match_arms = quote! {};
    for variant in &data.variants {
        if !variant.fields.is_empty() {
            return Err(syn::Error::new_spanned(
                variant,
                "BitCodec only supports c-like enums (variants cannot have fields)",
            ));
        }
        if variant.discriminant.is_none() {
            return Err(syn::Error::new_spanned(
                variant,
                "BitCodec requires explicit discriminant values for all variants",
            ));
        }
        let variant_ident = &variant.ident;
        match_arms.extend(quote! {
            x if x == #ident::#variant_ident as #cast_repr => #ident::#variant_ident,
        });
        try_match_arms.extend(quote! {
            x if x == #ident::#variant_ident as #cast_repr => Ok(#ident::#variant_ident),
        });
        write_match_arms.extend(quote! {
            #ident::#variant_ident => #ident::#variant_ident as #cast_repr,
        });
    }

    let variant_count = data.variants.len();
    let min_bits = if variant_count <= 1 {
        1
    } else {
        (usize::BITS - (variant_count - 1).leading_zeros()) as usize
    };

    // Build list of all storage types to implement for
    let mut all_types: Vec<(Type, bool, usize)> = Vec::new(); // (type, is_endian, bits)

    // ux2 types for bits < 8
    for bits in min_bits..8.min(max_bits + 1) {
        let ty_name = format!("u{}", bits);
        if let Ok(ty) = syn::parse_str::<Type>(&ty_name) {
            all_types.push((ty, false, bits));
        }
    }

    // u8
    if min_bits <= 8 && max_bits >= 8 {
        all_types.push((syn::parse_str("u8").unwrap(), false, 8));
    }

    // ux2 types 9-15 for repr(u16)
    if max_bits >= 16 {
        for bits in 9.max(min_bits)..16 {
            let ty_name = format!("u{}", bits);
            if let Ok(ty) = syn::parse_str::<Type>(&ty_name) {
                all_types.push((ty, false, bits));
            }
        }
    }

    // u16 + endian variants
    if min_bits <= 16 && max_bits >= 16 {
        all_types.push((syn::parse_str("u16").unwrap(), false, 16));
        all_types.push((syn::parse_str("u16_le").unwrap(), true, 16));
        all_types.push((syn::parse_str("u16_be").unwrap(), true, 16));
    }

    // ux2 types 17-31 for repr(u32)
    if max_bits >= 32 {
        for bits in 17.max(min_bits)..32 {
            let ty_name = format!("u{}", bits);
            if let Ok(ty) = syn::parse_str::<Type>(&ty_name) {
                all_types.push((ty, false, bits));
            }
        }
    }

    // Wider types
    if max_bits >= 8 {
        all_types.push((syn::parse_str("u32").unwrap(), false, 32));
        all_types.push((syn::parse_str("u32_le").unwrap(), true, 32));
        all_types.push((syn::parse_str("u32_be").unwrap(), true, 32));
        all_types.push((syn::parse_str("u64").unwrap(), false, 64));
        all_types.push((syn::parse_str("u64_le").unwrap(), true, 64));
        all_types.push((syn::parse_str("u64_be").unwrap(), true, 64));
    }

    let mut impls = quote! {};

    for (target_ty, _is_endian, bits) in &all_types {
        let bits = *bits;
        let is_fallible = if bits >= 64 {
            true
        } else {
            variant_count < (1usize << bits)
        };
        let read_type = if is_fallible {
            quote! { Result<Self, #target_ty> }
        } else {
            quote! { Self }
        };
        let read_impl = if is_fallible {
            quote! { <Self as BitCodec<#target_ty>>::try_read_bits(src, bit_offset) }
        } else {
            quote! { <Self as BitCodec<#target_ty>>::read_bits(src, bit_offset) }
        };

        // For the read path: extract the raw value for matching
        let read_val = quote! { (bits::read_u64(src, bit_offset, #bits) as #cast_repr) };

        // For the write path: convert enum to bits without requiring Copy.
        let write_val = quote! {
            match self {
                #write_match_arms
            } as u64
        };

        // try_read_bits for fallible
        let try_read_impl = if is_fallible {
            quote! {
                fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #target_ty> {
                    let raw = #read_val;
                    match raw {
                        #try_match_arms
                        _ => Err(<#target_ty as BitCodec<#target_ty>>::read_bits(src, bit_offset)),
                    }
                }
            }
        } else {
            // For infallible, try_read_bits delegates to read_bits (default)
            quote! {}
        };

        impls.extend(quote! {
            impl #impl_generics BitCodec<#target_ty> for #ident #type_generics #where_clause {
                const BITS: usize = #bits;
                const IS_FALLIBLE: bool = #is_fallible;
                type Read = #read_type;

                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    let raw = #read_val;
                    match raw {
                        #match_arms
                        _ => panic!("Invalid discriminant"),
                    }
                }

                #try_read_impl

                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    bits::write_u64(dst, bit_offset, #bits, #write_val);
                }

                fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                    #read_impl
                }
            }
        });
    }

    Ok(impls.into())
}

fn impl_zorua_struct(ast: &DeriveInput, data: &DataStruct) -> Result<TokenStream, syn::Error> {
    if data.fields.len() != 1 {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "BitCodec can only be derived for structs with exactly one field (newtypes)",
        ));
    }

    let field0 = data.fields.iter().next().unwrap();
    if field0.ident.is_some() {
        return Err(syn::Error::new_spanned(
            field0,
            "BitCodec can only be derived for tuple structs (use `struct Name(T)` syntax)",
        ));
    }

    if !get_repr_state(&ast.attrs)?.repr_transparent {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "BitCodec requires #[repr(transparent)] for newtype structs",
        ));
    }

    let wrapped_ty = &field0.ty;
    let ident = &ast.ident;
    let generic_params = &ast.generics.params;
    let (_, type_generics, where_clause) = ast.generics.split_for_impl();

    // Identity impl: BitCodec<Self> delegates to inner type's BitCodec<Inner>
    let identity_impl = quote! {
        impl<#generic_params> BitCodec<#ident #type_generics> for #ident #type_generics
        where
            #wrapped_ty: BitCodec<#wrapped_ty>,
            #where_clause
        {
            const BITS: usize = <#wrapped_ty as BitCodec<#wrapped_ty>>::BITS;
            const IS_FALLIBLE: bool = <#wrapped_ty as BitCodec<#wrapped_ty>>::IS_FALLIBLE;
            type Read = Self;

            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                Self(<#wrapped_ty as BitCodec<#wrapped_ty>>::read_bits(src, bit_offset))
            }

            fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #ident #type_generics> {
                <#wrapped_ty as BitCodec<#wrapped_ty>>::try_read_bits(src, bit_offset).map(Self).map_err(Self)
            }

            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                <#wrapped_ty as BitCodec<#wrapped_ty>>::write_bits(&self.0, dst, bit_offset);
            }

            fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                <Self as BitCodec<#ident #type_generics>>::read_bits(src, bit_offset)
            }
        }
    };

    // Delegation impls for known storage types.
    let mut delegation_impls = quote! {};

    if !ast.generics.params.is_empty() {
        // Generic newtypes: emit conditional impls for all known storage types.
        // The `where T: BitCodec<S>` bounds are not provably unsatisfied because `T`
        // is a type parameter, so Rust accepts them.
        let storage_types: &[&str] = &[
            "bool", "u1", "u2", "u3", "u4", "u5", "u6", "u7", "u8", "u9", "u10", "u11", "u12",
            "u13", "u14", "u15", "u16", "u17", "u18", "u19", "u20", "u21", "u22", "u23", "u24",
            "u25", "u26", "u27", "u28", "u29", "u30", "u31", "u32", "u64", "u16_le", "u16_be",
            "u32_le", "u32_be", "u64_le", "u64_be",
        ];

        for ty_name in storage_types {
            let storage_ty: Type = syn::parse_str(ty_name).unwrap();

            delegation_impls.extend(quote! {
                impl<#generic_params> BitCodec<#storage_ty> for #ident #type_generics
                where
                    #wrapped_ty: BitCodec<#storage_ty>,
                    #where_clause
                {
                    const BITS: usize = <#wrapped_ty as BitCodec<#storage_ty>>::BITS;
                    const IS_FALLIBLE: bool = <#wrapped_ty as BitCodec<#storage_ty>>::IS_FALLIBLE;
                    type Read = Self;

                    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                        Self(<#wrapped_ty as BitCodec<#storage_ty>>::read_bits(src, bit_offset))
                    }

                    fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #storage_ty> {
                        <#wrapped_ty as BitCodec<#storage_ty>>::try_read_bits(src, bit_offset).map(Self)
                    }

                    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                        <#wrapped_ty as BitCodec<#storage_ty>>::write_bits(&self.0, dst, bit_offset);
                    }

                    fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                        <Self as BitCodec<#storage_ty>>::read_bits(src, bit_offset)
                    }
                }
            });
        }
    } else {
        // Concrete newtypes: can't emit conditional impls for all storage types
        // because Rust rejects `where` clauses with provably-unsatisfied bounds
        // (issue #48214). However, for inner types that have endian cross-impls
        // (u16, u32, u64), we can emit a single generic impl parameterized by
        // `E: ByteOrder` — no `where` clause needed since the cross-impl always exists.
        use quote::format_ident;

        let inner_str = quote!(#wrapped_ty).to_string();
        let endian_wrapper = match inner_str.as_str() {
            "u16" => Some(format_ident!("U16")),
            "u32" => Some(format_ident!("U32")),
            "u64" => Some(format_ident!("U64")),
            _ => None,
        };

        if let Some(wrapper) = endian_wrapper {
            delegation_impls.extend(quote! {
                impl<E: ByteOrder> BitCodec<#wrapper<E>> for #ident {
                    const BITS: usize = <#wrapped_ty as BitCodec<#wrapper<E>>>::BITS;
                    const IS_FALLIBLE: bool = <#wrapped_ty as BitCodec<#wrapper<E>>>::IS_FALLIBLE;
                    type Read = Self;

                    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                        Self(<#wrapped_ty as BitCodec<#wrapper<E>>>::read_bits(src, bit_offset))
                    }

                    fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #wrapper<E>> {
                        <#wrapped_ty as BitCodec<#wrapper<E>>>::try_read_bits(src, bit_offset).map(Self)
                    }

                    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                        <#wrapped_ty as BitCodec<#wrapper<E>>>::write_bits(&self.0, dst, bit_offset);
                    }

                    fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                        <Self as BitCodec<#wrapper<E>>>::read_bits(src, bit_offset)
                    }
                }
            });
        }
    }

    let output = quote! {
        #identity_impl
        #delegation_impls
    };

    Ok(output.into())
}

// =====================================================================
// Helper functions
// =====================================================================

fn deconstruct_array(ty: &Type) -> Option<&Type> {
    if let Type::Array(ta) = ty {
        Some(&*ta.elem)
    } else {
        None
    }
}

fn extract_array_len(ty: &Type) -> Option<&Expr> {
    if let Type::Array(ta) = ty {
        Some(&ta.len)
    } else {
        None
    }
}

#[allow(unused)]
struct ReprState {
    repr_c: bool,
    repr_u8: bool,
    repr_u16: bool,
    repr_u32: bool,
    repr_transparent: bool,
    repr_packed: Option<usize>,
}

fn get_repr_state(attrs: &[Attribute]) -> Result<ReprState, syn::Error> {
    let mut repr_c = false;
    let mut repr_u8 = false;
    let mut repr_u16 = false;
    let mut repr_u32 = false;
    let mut repr_transparent = false;
    let mut repr_packed = None::<usize>;
    for attr in attrs {
        if attr.path().is_ident("repr") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("C") {
                    repr_c = true;
                    return Ok(());
                }
                if meta.path.is_ident("u8") {
                    repr_u8 = true;
                    return Ok(());
                }
                if meta.path.is_ident("u16") {
                    repr_u16 = true;
                    return Ok(());
                }
                if meta.path.is_ident("u32") {
                    repr_u32 = true;
                    return Ok(());
                }
                if meta.path.is_ident("transparent") {
                    repr_transparent = true;
                    return Ok(());
                }
                if meta.path.is_ident("packed") {
                    if meta.input.peek(token::Paren) {
                        let content;
                        syn::parenthesized!(content in meta.input);
                        let lit: LitInt = content.parse()?;
                        let n: usize = lit.base10_parse()?;
                        repr_packed = Some(n);
                    } else {
                        repr_packed = Some(1);
                    }
                    return Ok(());
                }
                Ok(())
            })?;
        }
    }

    Ok(ReprState {
        repr_c,
        repr_u8,
        repr_u16,
        repr_u32,
        repr_transparent,
        repr_packed,
    })
}

// =====================================================================
// bitstruct! proc macro
// =====================================================================

mod kw {
    syn::custom_keyword!(stride);
    syn::custom_keyword!(bits);
    syn::custom_keyword!(pad);
}

/// A field in the zorua struct
struct ZoruaFieldDef {
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    native_type: Type,
    storage_type: Type,
    has_backing_type: bool,
    bitfield_subfields: Option<Vec<BitfieldSubfield>>,
    padding_len: Option<Expr>,
}

struct BitfieldSubfield {
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    native_type: proc_macro2::TokenStream,
    storage_type: proc_macro2::TokenStream,
    has_backing_type: bool,
    is_zeroedoption: bool,
    bit_offset: Option<Expr>,
    stride: Option<Expr>,
    padding_bits: Option<Expr>,
    /// Compile-time assertion injected into getter bodies (empty if no check needed).
    offset_assertion: proc_macro2::TokenStream,
}

fn extract_storage_attr(attrs: &mut Vec<Attribute>) -> syn::Result<Option<Type>> {
    let mut storage = None;
    let mut retained = Vec::new();

    for attr in attrs.drain(..) {
        if !attr.path().is_ident("storage") {
            retained.push(attr);
            continue;
        }

        if storage.is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "duplicate #[storage(...)] attribute",
            ));
        }
        storage = Some(attr.parse_args()?);
    }

    *attrs = retained;
    Ok(storage)
}

impl Parse for ZoruaFieldDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut attrs = input.call(Attribute::parse_outer)?;

        if attrs.iter().any(|a| a.path().is_ident("fallible")) {
            return Err(syn::Error::new_spanned(
                attrs
                    .iter()
                    .find(|a| a.path().is_ident("fallible"))
                    .expect("fallible attr exists"),
                "`#[fallible]` is no longer used; Zorua infers Result-returning accessors from BitCodec",
            ));
        }

        attrs.retain(|a| !a.path().is_ident("zeroedoption"));
        let storage_attr = extract_storage_attr(&mut attrs)?;

        if input.peek(kw::pad) {
            input.parse::<kw::pad>()?;
            let padding_len: Expr = input.parse()?;
            return Ok(ZoruaFieldDef {
                attrs,
                vis: Visibility::Inherited,
                name: Ident::new("__zorua_pad", proc_macro2::Span::call_site()),
                native_type: syn::parse_str("()").unwrap(),
                storage_type: syn::parse_str("()").unwrap(),
                has_backing_type: false,
                bitfield_subfields: None,
                padding_len: Some(padding_len),
            });
        }

        let is_bits_container = input.peek(kw::bits);
        if is_bits_container {
            input.parse::<kw::bits>()?;
        }

        let vis: Visibility = if is_bits_container {
            Visibility::Inherited
        } else {
            input.parse()?
        };
        let name: Ident = input.parse()?;
        input.parse::<Token![:]>()?;

        let mut native_type_tokens = proc_macro2::TokenStream::new();
        let mut depth = 0;

        loop {
            if input.is_empty() {
                break;
            }
            if depth == 0
                && (input.peek(Token![as])
                    || input.peek(syn::token::Brace)
                    || input.peek(Token![,]))
            {
                break;
            }
            if input.peek(Token![<]) {
                depth += 1;
            } else if input.peek(Token![>]) {
                depth -= 1;
            }
            let tt: proc_macro2::TokenTree = input.parse()?;
            native_type_tokens.extend(std::iter::once(tt));
        }

        let native_type: Type = syn::parse2(native_type_tokens.clone()).map_err(|e| {
            syn::Error::new(name.span(), format!("Failed to parse native type: {}", e))
        })?;

        if input.peek(Token![as]) {
            return Err(syn::Error::new(
                name.span(),
                "`as` storage syntax has been replaced by #[storage(...)]",
            ));
        }

        let (storage_type, has_backing_type) = if let Some(storage) = storage_attr {
            (storage, true)
        } else {
            (native_type.clone(), false)
        };

        let bitfield_subfields = if input.peek(syn::token::Brace) {
            let content;
            braced!(content in input);
            let subfields: Punctuated<BitfieldSubfield, Token![,]> =
                content.parse_terminated(BitfieldSubfield::parse, Token![,])?;
            Some(subfields.into_iter().collect())
        } else {
            None
        };

        Ok(ZoruaFieldDef {
            attrs,
            vis,
            name,
            native_type,
            storage_type,
            has_backing_type,
            bitfield_subfields,
            padding_len: None,
        })
    }
}

impl Parse for BitfieldSubfield {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut attrs = input.call(Attribute::parse_outer)?;

        if attrs.iter().any(|a| a.path().is_ident("fallible")) {
            return Err(syn::Error::new_spanned(
                attrs
                    .iter()
                    .find(|a| a.path().is_ident("fallible"))
                    .expect("fallible attr exists"),
                "`#[fallible]` is no longer used; Zorua infers Result-returning accessors from BitCodec",
            ));
        }

        let is_zeroedoption = attrs.iter().any(|a| a.path().is_ident("zeroedoption"));
        attrs.retain(|a| !a.path().is_ident("zeroedoption"));
        let storage_attr = extract_storage_attr(&mut attrs)?;

        if input.peek(kw::pad) {
            input.parse::<kw::pad>()?;
            let padding_bits: Expr = input.parse()?;
            return Ok(BitfieldSubfield {
                attrs,
                vis: Visibility::Inherited,
                name: Ident::new("__zorua_pad", proc_macro2::Span::call_site()),
                native_type: quote! { () },
                storage_type: quote! { () },
                has_backing_type: false,
                is_zeroedoption: false,
                bit_offset: None,
                stride: None,
                padding_bits: Some(padding_bits),
                offset_assertion: proc_macro2::TokenStream::new(),
            });
        }

        let vis: Visibility = input.parse()?;
        let name: Ident = input.parse()?;

        input.parse::<Token![:]>()?;

        let mut native_type_tokens = proc_macro2::TokenStream::new();
        let mut depth = 0;

        loop {
            if input.is_empty() {
                break;
            }
            if depth == 0
                && (input.peek(Token![as]) || input.peek(Token![@]) || input.peek(Token![,]))
            {
                break;
            }
            if input.peek(Token![<]) {
                depth += 1;
            } else if input.peek(Token![>]) {
                depth -= 1;
            }
            let tt: proc_macro2::TokenTree = input.parse()?;
            native_type_tokens.extend(std::iter::once(tt));
        }

        if input.peek(Token![as]) {
            return Err(syn::Error::new(
                name.span(),
                "`as` storage syntax has been replaced by #[storage(...)]",
            ));
        }

        let (storage_type, has_backing_type) = if let Some(storage) = storage_attr {
            (quote! { #storage }, true)
        } else {
            (native_type_tokens.clone(), false)
        };

        let bit_offset = if input.peek(Token![@]) {
            input.parse::<Token![@]>()?;
            Some(input.parse::<Expr>()?)
        } else {
            None
        };

        let stride = if input.peek(kw::stride) {
            input.parse::<kw::stride>()?;
            Some(input.parse::<Expr>()?)
        } else {
            None
        };

        Ok(BitfieldSubfield {
            attrs,
            vis,
            name,
            native_type: native_type_tokens,
            storage_type,
            has_backing_type,
            is_zeroedoption,
            bit_offset,
            stride,
            padding_bits: None,
            offset_assertion: proc_macro2::TokenStream::new(),
        })
    }
}

struct ZoruaStructDef {
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    generics: syn::Generics,
    bits_annotation: Option<Expr>,
    fields: Vec<ZoruaFieldDef>,
}

impl Parse for ZoruaStructDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let vis: Visibility = input.parse()?;
        input.parse::<Token![struct]>()?;
        let name: Ident = input.parse()?;
        let generics: syn::Generics = input.parse()?;

        // Parse optional (bits EXPR) annotation
        let bits_annotation = if input.peek(token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            content.parse::<kw::bits>()?;
            Some(content.parse::<Expr>()?)
        } else {
            None
        };

        let content;
        braced!(content in input);
        let fields: Punctuated<ZoruaFieldDef, Token![,]> =
            content.parse_terminated(ZoruaFieldDef::parse, Token![,])?;

        Ok(ZoruaStructDef {
            attrs,
            vis,
            name,
            generics,
            bits_annotation,
            fields: fields.into_iter().collect(),
        })
    }
}

#[proc_macro]
pub fn bitstruct(item: TokenStream) -> TokenStream {
    let input = match syn::parse::<ZoruaStructDef>(item) {
        Ok(parsed) => parsed,
        Err(e) => {
            return e.into_compile_error().into();
        }
    };

    match generate_zorua_struct(input) {
        Ok(tokens) => tokens,
        Err(e) => e.into_compile_error().into(),
    }
}

fn generate_zorua_struct(input: ZoruaStructDef) -> Result<TokenStream, syn::Error> {
    let ZoruaStructDef {
        mut attrs,
        vis,
        name,
        generics,
        bits_annotation,
        mut fields,
    } = input;

    let default_endian = extract_default_endian(&mut attrs)?;
    for field in &mut fields {
        if field.padding_len.is_some() {
            continue;
        }

        if contains_native_multibyte(&field.storage_type) {
            if let Some(endian) = default_endian {
                let storage_type = endian_storage_type(&field.storage_type, endian).unwrap();
                field.storage_type = storage_type;
                if field.bitfield_subfields.is_none() {
                    field.has_backing_type = true;
                }
            } else {
                return Err(syn::Error::new_spanned(
                    &field.name,
                    "native multi-byte storage needs #[endian(little)] or #[endian(big)]",
                ));
            }
        }

        if let Some(subfields) = &mut field.bitfield_subfields {
            for subfield in subfields {
                if subfield.padding_bits.is_some() {
                    continue;
                }

                let storage_type: Type = syn::parse2(subfield.storage_type.clone())?;
                if contains_native_multibyte(&storage_type) {
                    if let Some(endian) = default_endian {
                        let storage_type = endian_storage_type(&storage_type, endian).unwrap();
                        subfield.storage_type = quote! { #storage_type };
                        subfield.has_backing_type = true;
                    } else {
                        return Err(syn::Error::new_spanned(
                            &subfield.name,
                            "native multi-byte storage needs #[endian(little)] or #[endian(big)]",
                        ));
                    }
                }
            }
        }
    }

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let injected_derives: Vec<Ident> = [
        "FromBytes",
        "IntoBytes",
        "KnownLayout",
        "Immutable",
        "Unaligned",
    ]
    .into_iter()
    .filter(|name| !has_derive(&attrs, name))
    .map(|name| Ident::new(name, proc_macro2::Span::call_site()))
    .collect();
    let injected_derive_attr = if injected_derives.is_empty() {
        quote! {}
    } else {
        quote! { #[derive(#(#injected_derives),*)] }
    };

    // Generate field definitions
    let field_defs: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(index, f)| {
            if let Some(len) = &f.padding_len {
                let field_name = syn::Ident::new(&format!("__zorua_pad_{index}"), f.name.span());
                return quote! {
                    #field_name: [u8; #len]
                };
            }

            let field_attrs = &f.attrs;
            let field_vis = if f.has_backing_type || f.bitfield_subfields.is_some() {
                Visibility::Inherited
            } else {
                f.vis.clone()
            };
            let field_name = f.name.clone();
            let field_ty = &f.storage_type;

            quote! {
                #(#field_attrs)*
                #field_vis #field_name: #field_ty
            }
        })
        .collect();

    // Generate flat backed field accessors
    let accessors: Vec<_> = fields
        .iter()
        .filter(|f| f.has_backing_type && f.padding_len.is_none())
        .map(|f| {
            let field_attrs = &f.attrs;
            let field_vis = &f.vis;
            let field_name = &f.name;
            let field_storage_name = &f.name;
            let field_setter = prefixed_name("set_", &f.name, "");
            let field_native_type = &f.native_type;
            let field_storage_type = &f.storage_type;

            let (getter, setter) = if let (Some(native_elem), Some(storage_elem)) = (deconstruct_array(field_native_type), deconstruct_array(field_storage_type)) {
                // Array flat backed field
                let len = extract_array_len(field_native_type).expect("array length required");
                let stride_expr = quote! { <#native_elem as BitCodec<#storage_elem>>::BITS };

                let getter = quote! {
                    #field_vis fn #field_name(&self) -> BitArrayView<'_, #native_elem, #storage_elem> {
                        BitArrayView::new(self.#field_storage_name.as_bytes(), 0, #len, #stride_expr)
                    }
                };

                let setter = quote! {
                    #field_vis fn #field_setter(&mut self) -> BitArrayViewMut<'_, #native_elem, #storage_elem> {
                        BitArrayViewMut::new(self.#field_storage_name.as_mut_bytes(), 0, #len, #stride_expr)
                    }
                };
                (getter, setter)
            } else {
                // Scalar flat backed field
                let getter = quote! {
                    #field_vis fn #field_name(&self) -> <#field_native_type as BitCodec<#field_storage_type>>::Read {
                        <#field_native_type as BitCodec<#field_storage_type>>::read(
                            self.#field_storage_name.as_bytes(), 0)
                    }
                };

                let setter = quote! {
                    #field_vis fn #field_setter(&mut self, val: #field_native_type) {
                        <#field_native_type as BitCodec<#field_storage_type>>::write_bits(
                            &val, self.#field_storage_name.as_mut_bytes(), 0);
                    }
                };
                (getter, setter)
            };

            quote! {
                #(#field_attrs)*
                #getter
                #(#field_attrs)*
                #setter
            }
        })
        .collect();

    // Generate raw accessors for non-array backed fields
    let raw_accessors: Vec<_> = fields
        .iter()
        .filter(|f| {
            f.has_backing_type
                && f.padding_len.is_none()
                && deconstruct_array(&f.native_type).is_none()
        })
        .map(|f| {
            let field_attrs = &f.attrs;
            let field_vis = &f.vis;
            let field_raw_name = syn::Ident::new(&format!("{}_raw", f.name), f.name.span());
            let field_raw_setter = prefixed_name("set_", &f.name, "_raw");
            let field_storage_type = &f.storage_type;
            let field_storage_name = &f.name;

            quote! {
                #(#field_attrs)*
                #field_vis fn #field_raw_name(&self) -> #field_storage_type {
                    self.#field_storage_name
                }

                #(#field_attrs)*
                #field_vis fn #field_raw_setter(&mut self, val: #field_storage_type) {
                    self.#field_storage_name = val;
                }
            }
        })
        .collect();

    // For concrete structs, emit eager top-level bit coverage assertions so
    // incomplete bitfields fail even when no accessor is referenced. Generic
    // bitfield coverage is still checked in generated accessors; stable Rust
    // cannot express the same eager assertion for arbitrary generic consts.
    let bitfield_assertions: Vec<_> = if generics.params.is_empty() {
        fields
            .iter()
            .filter_map(|f| {
                f.bitfield_subfields.as_ref().map(|subfields| {
                    let container_type = &f.storage_type;
                    let mut current_offset: proc_macro2::TokenStream = quote! { 0usize };

                    for sf in subfields {
                        if let Some(ref explicit) = sf.bit_offset {
                            current_offset = quote! { #explicit };
                        }
                        let bits_expr = field_bits_expr(sf);
                        let prev = current_offset.clone();
                        current_offset = quote! { #prev + #bits_expr };
                    }

                    quote! {
                        const _: () = {
                            assert!(
                                #current_offset == <#container_type as BitCodec<#container_type>>::BITS,
                                "bitfield does not account for every bit in its storage"
                            );
                        };
                    }
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    // Generate bitfield subfield accessors with sequential offset resolution.
    let bitfield_accessors: Vec<_> = fields
        .iter()
        .filter_map(|f| {
            f.bitfield_subfields.as_ref().map(|subfields| {
                let container_name = f.name.clone();
                let container_type = &f.storage_type;
                let assert_name = syn::Ident::new(
                    &format!("__ZORUA_ASSERT_BITS_{}", f.name).to_uppercase(),
                    f.name.span(),
                );

                // Resolve offsets: track cumulative position as a token stream.
                // Fields with explicit @offset reset the position; fields without
                // are placed sequentially after the previous field.
                let mut current_offset: proc_macro2::TokenStream = quote! { 0usize };

                let mut generated = Vec::new();

                for sf in subfields {
                    let resolved_offset = current_offset.clone();
                    let offset_assertion = if let Some(ref explicit) = sf.bit_offset {
                        let expected = current_offset.clone();
                        quote! {
                            const { assert!(
                                #explicit == #expected,
                                "explicit bit offset does not match sequential layout"
                            ) };
                        }
                    } else {
                        quote! {}
                    };

                    let bits_expr = field_bits_expr(sf);
                    let prev = current_offset.clone();
                    current_offset = quote! { #prev + #bits_expr };

                    if sf.padding_bits.is_none() {
                        generated.push(generate_subfield_accessor_with_assert(
                            &container_name,
                            sf,
                            &resolved_offset,
                            &offset_assertion,
                        ));
                    }
                }

                generated.push(quote! {
                    const #assert_name: () = {
                        assert!(
                            #current_offset == <#container_type as BitCodec<#container_type>>::BITS,
                            "bitfield does not account for every bit in its storage"
                        );
                    };
                });

                generated
            })
        })
        .flatten()
        .collect();

    // Generate BitCodec<Self> impl from bits annotation
    let bits_impl = if let Some(ref bits_expr) = bits_annotation {
        quote! {
            impl #impl_generics BitCodec<#name #ty_generics> for #name #ty_generics #where_clause {
                const BITS: usize = #bits_expr;
                const IS_FALLIBLE: bool = false;
                type Read = Self;

                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    let mut s = Self::new_zeroed();
                    bits::copy(src, bit_offset, s.as_mut_bytes(), 0, #bits_expr);
                    s
                }

                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    bits::copy(self.as_bytes(), 0, dst, bit_offset, #bits_expr);
                }

                fn read(src: &[u8], bit_offset: usize) -> Self::Read {
                    <Self as BitCodec<#name #ty_generics>>::read_bits(src, bit_offset)
                }
            }
        }
    } else {
        quote! {}
    };

    let output = quote! {
        #(#attrs)*
        #injected_derive_attr
        #vis struct #name #generics #where_clause {
            #(#field_defs),*
        }

        impl #impl_generics #name #ty_generics #where_clause {
            #(#accessors)*
            #(#raw_accessors)*
            #(#bitfield_accessors)*
        }

        #(#bitfield_assertions)*
        #bits_impl
    };

    Ok(output.into())
}

/// Returns a token stream for the BITS of a subfield (used for sequential offset tracking).
fn field_bits_expr(sf: &BitfieldSubfield) -> proc_macro2::TokenStream {
    if let Some(bits) = &sf.padding_bits {
        return quote! { #bits };
    }

    let native_ts = &sf.native_type;
    let storage_ts = &sf.storage_type;

    let native_type: Type = syn::parse2(native_ts.clone()).unwrap();

    if let Some(native_elem) = deconstruct_array(&native_type) {
        // Array: N * element BITS
        let len = extract_array_len(&native_type).unwrap();
        let storage_type: Type = syn::parse2(storage_ts.clone()).unwrap();
        let storage_elem = if sf.has_backing_type {
            deconstruct_array(&storage_type).unwrap()
        } else {
            native_elem
        };
        if let Some(ref stride) = sf.stride {
            quote! { #len * (#stride) }
        } else {
            quote! { #len * <#native_elem as BitCodec<#storage_elem>>::BITS }
        }
    } else {
        // Scalar
        if sf.has_backing_type {
            quote! { <#native_ts as BitCodec<#storage_ts>>::BITS }
        } else {
            quote! { <#native_ts as BitCodec<#native_ts>>::BITS }
        }
    }
}

/// Wrapper that injects a resolved offset and optional compile-time assertion
/// into the generated accessor code.
fn generate_subfield_accessor_with_assert(
    container_name: &Ident,
    sf: &BitfieldSubfield,
    resolved_offset: &proc_macro2::TokenStream,
    assertion: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    // Create a temporary subfield with the resolved offset.
    let resolved = BitfieldSubfield {
        attrs: sf.attrs.clone(),
        vis: sf.vis.clone(),
        name: sf.name.clone(),
        native_type: sf.native_type.clone(),
        storage_type: sf.storage_type.clone(),
        has_backing_type: sf.has_backing_type,
        is_zeroedoption: sf.is_zeroedoption,
        bit_offset: Some(syn::parse2(resolved_offset.clone()).unwrap()),
        stride: sf.stride.clone(),
        padding_bits: sf.padding_bits.clone(),
        offset_assertion: assertion.clone(),
    };
    generate_subfield_accessor(container_name, &resolved)
}

/// Generate accessor methods for a bitfield subfield.
fn generate_subfield_accessor(
    container_name: &Ident,
    sf: &BitfieldSubfield,
) -> proc_macro2::TokenStream {
    let sf_native_type_ts = &sf.native_type;
    let sf_storage_type_ts = &sf.storage_type;

    let sf_native_type: Type = syn::parse2(sf_native_type_ts.clone()).unwrap();
    let sf_storage_type: Type = syn::parse2(sf_storage_type_ts.clone()).unwrap();

    let is_identity =
        quote!(#sf_native_type_ts).to_string() == quote!(#sf_storage_type_ts).to_string();

    if let Some(native_elem) = deconstruct_array(&sf_native_type) {
        // Array subfield
        let storage_elem = if sf.has_backing_type {
            deconstruct_array(&sf_storage_type).unwrap()
        } else {
            native_elem
        };
        let elem_is_identity =
            quote!(#native_elem).to_string() == quote!(#storage_elem).to_string();

        if sf.is_zeroedoption {
            generate_zeroedoption_array_subfield_accessor(
                container_name,
                sf,
                native_elem,
                storage_elem,
                elem_is_identity,
            )
        } else {
            generate_array_subfield_accessor(
                container_name,
                sf,
                native_elem,
                storage_elem,
                elem_is_identity,
            )
        }
    } else if sf.is_zeroedoption {
        // Zeroedoption subfield
        generate_zeroedoption_subfield_accessor(container_name, sf, is_identity)
    } else {
        // Scalar subfield
        generate_scalar_subfield_accessor(container_name, sf, is_identity)
    }
}

fn generate_scalar_subfield_accessor(
    container_name: &Ident,
    sf: &BitfieldSubfield,
    is_identity: bool,
) -> proc_macro2::TokenStream {
    let sf_attrs = &sf.attrs;
    let sf_vis = &sf.vis;
    let sf_name = &sf.name;
    let sf_setter = prefixed_name("set_", &sf.name, "");
    let sf_native_type_ts = &sf.native_type;
    let sf_storage_type_ts = &sf.storage_type;
    let sf_offset = sf.bit_offset.as_ref().expect("resolved offset required");
    let offset_assertion = &sf.offset_assertion;

    if sf.has_backing_type {
        // With `as` — has raw accessors
        let sf_raw_name = syn::Ident::new(&format!("{}_raw", sf.name), sf.name.span());
        let sf_raw_setter = prefixed_name("set_", &sf.name, "_raw");

        let getter = if is_identity {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::Read {
                    #offset_assertion
                    self.#sf_raw_name()
                }
            }
        } else {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::Read {
                    #offset_assertion
                    <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::read(
                        self.#container_name.as_bytes(), #sf_offset)
                }
            }
        };

        let setter = if is_identity {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_setter(&mut self, val: #sf_native_type_ts) {
                    self.#sf_raw_setter(val);
                }
            }
        } else {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_setter(&mut self, val: #sf_native_type_ts) {
                    <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::write_bits(
                        &val, self.#container_name.as_mut_bytes(), #sf_offset);
                }
            }
        };

        quote! {
            #getter
            #setter

            #(#sf_attrs)*
            #sf_vis fn #sf_raw_name(&self) -> #sf_storage_type_ts {
                <#sf_storage_type_ts as BitCodec<#sf_storage_type_ts>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset)
            }

            #(#sf_attrs)*
            #sf_vis fn #sf_raw_setter(&mut self, val: #sf_storage_type_ts) {
                <#sf_storage_type_ts as BitCodec<#sf_storage_type_ts>>::write_bits(
                    &val, self.#container_name.as_mut_bytes(), #sf_offset);
            }
        }
    } else {
        // No `as` — identity access, no raw accessors needed
        let getter = quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_name(&self) -> <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::Read {
                #offset_assertion
                <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::read(
                    self.#container_name.as_bytes(), #sf_offset)
            }
        };

        let setter = quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_setter(&mut self, val: #sf_native_type_ts) {
                <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::write_bits(
                    &val, self.#container_name.as_mut_bytes(), #sf_offset);
            }
        };

        quote! {
            #getter
            #setter
        }
    }
}

fn generate_zeroedoption_subfield_accessor(
    container_name: &Ident,
    sf: &BitfieldSubfield,
    is_identity: bool,
) -> proc_macro2::TokenStream {
    let sf_attrs = &sf.attrs;
    let sf_vis = &sf.vis;
    let sf_name = &sf.name;
    let sf_setter = prefixed_name("set_", &sf.name, "");
    let sf_native_type_ts = &sf.native_type;
    let sf_storage_type_ts = &sf.storage_type;
    let sf_offset = sf.bit_offset.as_ref().expect("resolved offset required");
    let offset_assertion = &sf.offset_assertion;

    let sf_raw_name = syn::Ident::new(&format!("{}_raw", sf.name), sf.name.span());
    let sf_raw_setter = prefixed_name("set_", &sf.name, "_raw");

    let bits_expr = if is_identity {
        quote! { <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::BITS }
    } else {
        quote! { <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::BITS }
    };

    let read_native = if is_identity {
        quote! { <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    } else {
        quote! { <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    };

    let write_native = if is_identity {
        quote! { <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::write_bits(&v, self.#container_name.as_mut_bytes(), #sf_offset) }
    } else {
        quote! { <#sf_native_type_ts as BitCodec<#sf_storage_type_ts>>::write_bits(&v, self.#container_name.as_mut_bytes(), #sf_offset) }
    };

    let read_raw = if is_identity {
        quote! { <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    } else {
        quote! { <#sf_storage_type_ts as BitCodec<#sf_storage_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    };

    let raw_type = if is_identity {
        sf_native_type_ts.clone()
    } else {
        sf_storage_type_ts.clone()
    };

    quote! {
        #(#sf_attrs)*
        #sf_vis fn #sf_name(&self) -> Option<#sf_native_type_ts> {
            #offset_assertion
            if bits::are_zero(self.#container_name.as_bytes(), #sf_offset, #bits_expr) {
                None
            } else {
                Some(#read_native)
            }
        }

        #(#sf_attrs)*
        #sf_vis fn #sf_setter(&mut self, val: Option<#sf_native_type_ts>) {
            match val {
                None => {
                    // Zero out the bits
                    bits::write_u64(self.#container_name.as_mut_bytes(), #sf_offset, #bits_expr, 0);
                }
                Some(v) => {
                    #write_native;
                }
            }
        }

        #(#sf_attrs)*
        #sf_vis fn #sf_raw_name(&self) -> #raw_type {
            #read_raw
        }

        #(#sf_attrs)*
        #sf_vis fn #sf_raw_setter(&mut self, val: #raw_type) {
            <#raw_type as BitCodec<#raw_type>>::write_bits(&val, self.#container_name.as_mut_bytes(), #sf_offset);
        }
    }
}

fn generate_zeroedoption_array_subfield_accessor(
    container_name: &Ident,
    sf: &BitfieldSubfield,
    native_elem: &Type,
    storage_elem: &Type,
    elem_is_identity: bool,
) -> proc_macro2::TokenStream {
    let sf_attrs = &sf.attrs;
    let sf_vis = &sf.vis;
    let sf_name = &sf.name;
    let sf_mut = prefixed_name("", &sf.name, "_mut");
    let sf_offset = sf.bit_offset.as_ref().expect("resolved offset required");
    let offset_assertion = &sf.offset_assertion;
    let native_type: Type = syn::parse2(sf.native_type.clone()).unwrap();
    let len = extract_array_len(&native_type).expect("array length required");

    // Determine stride and element BITS
    let (stride_expr, bits_expr) = if elem_is_identity {
        (
            quote! { <#native_elem as BitCodec<#native_elem>>::BITS },
            quote! { <#native_elem as BitCodec<#native_elem>>::BITS },
        )
    } else {
        (
            quote! { <#native_elem as BitCodec<#storage_elem>>::BITS },
            quote! { <#native_elem as BitCodec<#storage_elem>>::BITS },
        )
    };

    quote! {
        #(#sf_attrs)*
        #sf_vis fn #sf_name(&self) -> ZeroedOptionBitArrayView<'_, #native_elem, #storage_elem> {
            #offset_assertion
            ZeroedOptionBitArrayView::new(
                self.#container_name.as_bytes(),
                #sf_offset,
                #len,
                #stride_expr,
                #bits_expr,
            )
        }

        #(#sf_attrs)*
        #sf_vis fn #sf_mut(&mut self) -> ZeroedOptionBitArrayViewMut<'_, #native_elem, #storage_elem> {
            ZeroedOptionBitArrayViewMut::new(
                self.#container_name.as_mut_bytes(),
                #sf_offset,
                #len,
                #stride_expr,
                #bits_expr,
            )
        }
    }
}

fn generate_array_subfield_accessor(
    container_name: &Ident,
    sf: &BitfieldSubfield,
    native_elem: &Type,
    storage_elem: &Type,
    elem_is_identity: bool,
) -> proc_macro2::TokenStream {
    let sf_attrs = &sf.attrs;
    let sf_vis = &sf.vis;
    let sf_name = &sf.name;
    let sf_mut = prefixed_name("", &sf.name, "_mut");
    let sf_native_type_ts = &sf.native_type;
    let sf_storage_type_ts = &sf.storage_type;
    let sf_offset = sf.bit_offset.as_ref().expect("resolved offset required");
    let offset_assertion = &sf.offset_assertion;
    let native_type: Type = syn::parse2(sf.native_type.clone()).unwrap();
    let len = extract_array_len(&native_type).expect("array length required");

    // Determine stride: explicit `stride` keyword, or from Zorua BITS
    let stride_expr = if let Some(ref stride) = sf.stride {
        quote! { #stride }
    } else if elem_is_identity {
        quote! { <#native_elem as BitCodec<#native_elem>>::BITS }
    } else {
        quote! { <#native_elem as BitCodec<#storage_elem>>::BITS }
    };

    let getter = quote! {
        #(#sf_attrs)*
        #sf_vis fn #sf_name(&self) -> BitArrayView<'_, #native_elem, #storage_elem> {
            #offset_assertion
            BitArrayView::new(self.#container_name.as_bytes(), #sf_offset, #len, #stride_expr)
        }
    };

    let setter = quote! {
        #(#sf_attrs)*
        #sf_vis fn #sf_mut(&mut self) -> BitArrayViewMut<'_, #native_elem, #storage_elem> {
            BitArrayViewMut::new(self.#container_name.as_mut_bytes(), #sf_offset, #len, #stride_expr)
        }
    };

    // Raw whole-array accessors (skip when stride is explicit)
    let raw_accessors = if sf.stride.is_some() {
        quote! {}
    } else if sf.has_backing_type {
        let sf_raw_name = syn::Ident::new(&format!("{}_raw", sf.name), sf.name.span());
        let sf_raw_setter = prefixed_name("set_", &sf.name, "_raw");
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_name(&self) -> #sf_storage_type_ts {
                <#sf_storage_type_ts as BitCodec<#sf_storage_type_ts>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset)
            }
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_setter(&mut self, val: #sf_storage_type_ts) {
                <#sf_storage_type_ts as BitCodec<#sf_storage_type_ts>>::write_bits(
                    &val, self.#container_name.as_mut_bytes(), #sf_offset);
            }
        }
    } else {
        let sf_raw_name = syn::Ident::new(&format!("{}_raw", sf.name), sf.name.span());
        let sf_raw_setter = prefixed_name("set_", &sf.name, "_raw");
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_name(&self) -> #sf_native_type_ts {
                <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset)
            }
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_setter(&mut self, val: #sf_native_type_ts) {
                <#sf_native_type_ts as BitCodec<#sf_native_type_ts>>::write_bits(
                    &val, self.#container_name.as_mut_bytes(), #sf_offset);
            }
        }
    };

    quote! {
        #getter
        #setter
        #raw_accessors
    }
}
