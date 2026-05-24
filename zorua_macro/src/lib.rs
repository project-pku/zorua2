use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Attribute, Data, DataEnum, DataStruct, DeriveInput, Expr, Ident, LitInt, Token, Type,
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

// =====================================================================
// derive(ZoruaStruct) — renamed from derive(ZoruaField)
// =====================================================================

/// Derive macro for `ZoruaStruct` (POD types safe to transmute).
///
/// Composite structs must have `#[repr(C)]`.
/// Enums must have `#[repr(u8)]` and exactly 256 variants.
#[proc_macro_derive(ZoruaStruct)]
pub fn zorua_struct_derive_macro(item: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(item).unwrap();
    let result = match &ast.data {
        Data::Struct(data) => impl_zoruastruct_struct(&ast, data),
        Data::Enum(data) => impl_zoruastruct_enum(&ast, data),
        _ => Err(syn::Error::new_spanned(
            &ast,
            "ZoruaStruct can only be derived for structs and enums",
        )),
    };
    result.unwrap_or_else(|e| e.into_compile_error().into())
}

fn impl_zoruastruct_struct(
    ast: &DeriveInput,
    data: &DataStruct,
) -> Result<TokenStream, syn::Error> {
    if (data.fields.len() > 1) && !get_repr_state(&ast.attrs)?.repr_c {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "Composite structs must have the C repr to derive ZoruaStruct. \
            Try adding `#[repr(C)]` to the struct.",
        ));
    }
    Ok(generate_impl("ZoruaStruct", true, ast, None, None))
}

fn impl_zoruastruct_enum(ast: &DeriveInput, data: &DataEnum) -> Result<TokenStream, syn::Error> {
    if !get_repr_state(&ast.attrs)?.repr_u8 {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "Enums must have the u8 repr to derive ZoruaStruct. \
            Try adding `#[repr(u8)]` to the enum.",
        ));
    }
    if data.variants.len() != 256 {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            format!(
                "Enums must have exactly 256 variants to derive ZoruaStruct (found {}).",
                data.variants.len()
            ),
        ));
    }
    Ok(generate_impl("ZoruaStruct", true, ast, None, None))
}

// =====================================================================
// derive(Zorua) — bit read/write trait for enums and newtypes
// =====================================================================

/// Derive macro that implements `Zorua<S>` trait for enums and newtype structs.
///
/// For enums: generates identity impl + wider storage impls.
/// For newtypes: generates identity impl + delegation impls per known storage type.
#[proc_macro_derive(Zorua)]
pub fn zorua_derive_macro(item: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(item).unwrap();
    let result = match &ast.data {
        Data::Enum(data) => impl_zorua_enum(&ast, data),
        Data::Struct(data) => impl_zorua_struct(&ast, data),
        _ => Err(syn::Error::new_spanned(
            &ast,
            "Zorua can only be derived for enums and structs",
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
            "Zorua requires #[repr(u8)], #[repr(u16)], or #[repr(u32)] for enums",
        ));
    };

    let ident = &ast.ident;
    let (impl_generics, type_generics, where_clause) = ast.generics.split_for_impl();

    // Validate variants and build match arms
    let mut match_arms = quote! {};
    let mut try_match_arms = quote! {};
    for variant in &data.variants {
        if !variant.fields.is_empty() {
            return Err(syn::Error::new_spanned(
                variant,
                "Zorua only supports c-like enums (variants cannot have fields)",
            ));
        }
        if variant.discriminant.is_none() {
            return Err(syn::Error::new_spanned(
                variant,
                "Zorua requires explicit discriminant values for all variants",
            ));
        }
        let variant_ident = &variant.ident;
        match_arms.extend(quote! {
            x if x == #ident::#variant_ident as #cast_repr => #ident::#variant_ident,
        });
        try_match_arms.extend(quote! {
            x if x == #ident::#variant_ident as #cast_repr => Ok(#ident::#variant_ident),
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

        // For the read path: extract the raw value for matching
        let read_val = quote! { (bits::read_u64(src, bit_offset, #bits) as #cast_repr) };

        // For the write path: convert enum to bits
        // Use unsafe transmute of the discriminant to handle non-Copy enums
        let write_val = quote! {
            unsafe { *<*const Self>::cast::<#cast_repr>(self) as u64 }
        };

        // try_read_bits for fallible
        let try_read_impl = if is_fallible {
            quote! {
                fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #target_ty> {
                    let raw = #read_val;
                    match raw {
                        #try_match_arms
                        _ => Err(<#target_ty as Zorua<#target_ty>>::read_bits(src, bit_offset)),
                    }
                }
            }
        } else {
            // For infallible, try_read_bits delegates to read_bits (default)
            quote! {}
        };

        impls.extend(quote! {
            impl #impl_generics Zorua<#target_ty> for #ident #type_generics #where_clause {
                const BITS: usize = #bits;
                const IS_FALLIBLE: bool = #is_fallible;

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
            }
        });
    }

    Ok(impls.into())
}

fn impl_zorua_struct(ast: &DeriveInput, data: &DataStruct) -> Result<TokenStream, syn::Error> {
    if data.fields.len() != 1 {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "Zorua can only be derived for structs with exactly one field (newtypes)",
        ));
    }

    let field0 = data.fields.iter().next().unwrap();
    if field0.ident.is_some() {
        return Err(syn::Error::new_spanned(
            field0,
            "Zorua can only be derived for tuple structs (use `struct Name(T)` syntax)",
        ));
    }

    if !get_repr_state(&ast.attrs)?.repr_transparent {
        return Err(syn::Error::new_spanned(
            &ast.ident,
            "Zorua requires #[repr(transparent)] for newtype structs",
        ));
    }

    let wrapped_ty = &field0.ty;
    let ident = &ast.ident;
    let generic_params = &ast.generics.params;
    let (_, type_generics, where_clause) = ast.generics.split_for_impl();

    // Identity impl: Zorua<Self> delegates to inner type's Zorua<Inner>
    let identity_impl = quote! {
        impl<#generic_params> Zorua<#ident #type_generics> for #ident #type_generics
        where
            #wrapped_ty: Zorua<#wrapped_ty>,
            #where_clause
        {
            const BITS: usize = <#wrapped_ty as Zorua<#wrapped_ty>>::BITS;
            const IS_FALLIBLE: bool = <#wrapped_ty as Zorua<#wrapped_ty>>::IS_FALLIBLE;

            fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                Self(<#wrapped_ty as Zorua<#wrapped_ty>>::read_bits(src, bit_offset))
            }

            fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #ident #type_generics> {
                <#wrapped_ty as Zorua<#wrapped_ty>>::try_read_bits(src, bit_offset).map(Self).map_err(Self)
            }

            fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                <#wrapped_ty as Zorua<#wrapped_ty>>::write_bits(&self.0, dst, bit_offset);
            }
        }
    };

    // Delegation impls for known storage types.
    let mut delegation_impls = quote! {};

    if !ast.generics.params.is_empty() {
        // Generic newtypes: emit conditional impls for all known storage types.
        // The `where T: Zorua<S>` bounds are not provably unsatisfied because `T`
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
                impl<#generic_params> Zorua<#storage_ty> for #ident #type_generics
                where
                    #wrapped_ty: Zorua<#storage_ty>,
                    #where_clause
                {
                    const BITS: usize = <#wrapped_ty as Zorua<#storage_ty>>::BITS;
                    const IS_FALLIBLE: bool = <#wrapped_ty as Zorua<#storage_ty>>::IS_FALLIBLE;

                    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                        Self(<#wrapped_ty as Zorua<#storage_ty>>::read_bits(src, bit_offset))
                    }

                    fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #storage_ty> {
                        <#wrapped_ty as Zorua<#storage_ty>>::try_read_bits(src, bit_offset).map(Self)
                    }

                    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                        <#wrapped_ty as Zorua<#storage_ty>>::write_bits(&self.0, dst, bit_offset);
                    }
                }
            });
        }
    } else {
        // Concrete newtypes: can't emit conditional impls for all storage types
        // because Rust rejects `where` clauses with provably-unsatisfied bounds
        // (issue #48214). However, for inner types that have endian cross-impls
        // (u16, u32, u64), we can emit a single generic impl parameterized by
        // `E: Endian` — no `where` clause needed since the cross-impl always exists.
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
                impl<E: Endian> Zorua<#wrapper<E>> for #ident {
                    const BITS: usize = <#wrapped_ty as Zorua<#wrapper<E>>>::BITS;
                    const IS_FALLIBLE: bool = <#wrapped_ty as Zorua<#wrapper<E>>>::IS_FALLIBLE;

                    fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                        Self(<#wrapped_ty as Zorua<#wrapper<E>>>::read_bits(src, bit_offset))
                    }

                    fn try_read_bits(src: &[u8], bit_offset: usize) -> Result<Self, #wrapper<E>> {
                        <#wrapped_ty as Zorua<#wrapper<E>>>::try_read_bits(src, bit_offset).map(Self)
                    }

                    fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                        <#wrapped_ty as Zorua<#wrapper<E>>>::write_bits(&self.0, dst, bit_offset);
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

fn generate_impl(
    ty: &str,
    is_unsafe: bool,
    ast: &DeriveInput,
    tokens: Option<proc_macro2::TokenStream>,
    asserts: Option<proc_macro2::TokenStream>,
) -> TokenStream {
    let ty: Type = syn::parse_str(ty).unwrap();
    let ident = &ast.ident;
    let (impl_generics, type_generics, where_clause) = ast.generics.split_for_impl();
    let unsafe_keyword = is_unsafe.then_some(quote! {unsafe});
    quote! {
        #unsafe_keyword impl #impl_generics #ty for #ident #type_generics #where_clause {
            #tokens
        }

        impl #impl_generics #ident #type_generics #where_clause {
            #asserts
        }
    }
    .into()
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
// bitfields! proc macro
// =====================================================================

mod kw {
    syn::custom_keyword!(stride);
    syn::custom_keyword!(bits);
}

/// A field in the zorua struct
struct ZoruaFieldDef {
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    native_type: Type,
    storage_type: Type,
    has_backing_type: bool,
    is_fallible: bool,
    bitfield_subfields: Option<Vec<BitfieldSubfield>>,
}

struct BitfieldSubfield {
    attrs: Vec<Attribute>,
    vis: Visibility,
    name: Ident,
    native_type: proc_macro2::TokenStream,
    storage_type: proc_macro2::TokenStream,
    has_backing_type: bool,
    is_fallible: bool,
    is_zeroedoption: bool,
    bit_offset: Option<Expr>,
    stride: Option<Expr>,
    /// Compile-time assertion injected into getter bodies (empty if no check needed).
    offset_assertion: proc_macro2::TokenStream,
}

impl Parse for ZoruaFieldDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let all_attrs = input.call(Attribute::parse_outer)?;

        let is_fallible = all_attrs.iter().any(|a| a.path().is_ident("fallible"));

        let attrs: Vec<_> = all_attrs
            .into_iter()
            .filter(|a| !a.path().is_ident("fallible") && !a.path().is_ident("zeroedoption"))
            .collect();

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

        let (storage_type, has_backing_type) = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;
            let storage: Type = input.parse()?;
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

        if is_fallible && !has_backing_type {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "Field `{}` marked #[fallible] but has no `as` storage type, \
                     so the annotation has no effect.",
                    name
                ),
            ));
        }

        Ok(ZoruaFieldDef {
            attrs,
            vis,
            name,
            native_type,
            storage_type,
            has_backing_type,
            is_fallible,
            bitfield_subfields,
        })
    }
}

impl Parse for BitfieldSubfield {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let all_attrs = input.call(Attribute::parse_outer)?;

        let is_fallible = all_attrs.iter().any(|a| a.path().is_ident("fallible"));
        let is_zeroedoption = all_attrs.iter().any(|a| a.path().is_ident("zeroedoption"));

        let attrs: Vec<_> = all_attrs
            .into_iter()
            .filter(|a| !a.path().is_ident("fallible") && !a.path().is_ident("zeroedoption"))
            .collect();

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

        let (storage_type, has_backing_type) = if input.peek(Token![as]) {
            input.parse::<Token![as]>()?;

            let mut storage_tokens = proc_macro2::TokenStream::new();
            let mut depth = 0;

            loop {
                if input.is_empty() {
                    break;
                }
                if depth == 0 && (input.peek(Token![@]) || input.peek(Token![,])) {
                    break;
                }
                if input.peek(Token![<]) {
                    depth += 1;
                } else if input.peek(Token![>]) {
                    depth -= 1;
                }
                let tt: proc_macro2::TokenTree = input.parse()?;
                storage_tokens.extend(std::iter::once(tt));
            }

            (storage_tokens, true)
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

        if is_fallible && !has_backing_type {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "Subfield `{}` marked #[fallible] but has no `as` storage type, \
                     so the annotation has no effect.",
                    name
                ),
            ));
        }

        Ok(BitfieldSubfield {
            attrs,
            vis,
            name,
            native_type: native_type_tokens,
            storage_type,
            has_backing_type,
            is_fallible,
            is_zeroedoption,
            bit_offset,
            stride,
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
pub fn bitfields(item: TokenStream) -> TokenStream {
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
        attrs,
        vis,
        name,
        generics,
        bits_annotation,
        fields,
    } = input;

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // Generate field definitions
    let field_defs: Vec<_> = fields
        .iter()
        .map(|f| {
            let field_attrs = &f.attrs;
            let field_vis = &f.vis;
            let field_name = if f.has_backing_type {
                syn::Ident::new(&format!("{}_raw", f.name), f.name.span())
            } else {
                f.name.clone()
            };
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
        .filter(|f| f.has_backing_type)
        .map(|f| {
            let field_attrs = &f.attrs;
            let field_vis = &f.vis;
            let field_name = &f.name;
            let field_raw_name = syn::Ident::new(&format!("{}_raw", f.name), f.name.span());
            let field_setter = prefixed_name("set_", &f.name, "");
            let field_raw_setter = prefixed_name("set_", &f.name, "_raw");
            let field_native_type = &f.native_type;
            let field_storage_type = &f.storage_type;

            let (getter, setter) = if let (Some(native_elem), Some(storage_elem)) = (deconstruct_array(field_native_type), deconstruct_array(field_storage_type)) {
                // Array flat backed field
                let is_identity = quote!(#native_elem).to_string() == quote!(#storage_elem).to_string();

                let assertion = gen_fallibility_assertion(f.is_fallible, is_identity, field_name, &quote!(#native_elem), &quote!(#storage_elem));

                let getter = if is_identity {
                    quote! {
                        #field_vis fn #field_name(&self, index: usize) -> #native_elem {
                            #assertion
                            self.#field_raw_name[index]
                        }
                    }
                } else if f.is_fallible {
                    quote! {
                        #field_vis fn #field_name(&self, index: usize) -> Result<#native_elem, #storage_elem> {
                            #assertion
                            <#native_elem as Zorua<#storage_elem>>::try_read_bits(
                                self.#field_raw_name[index].as_bytes(), 0)
                        }
                    }
                } else {
                    quote! {
                        #field_vis fn #field_name(&self, index: usize) -> #native_elem {
                            #assertion
                            <#native_elem as Zorua<#storage_elem>>::read_bits(
                                self.#field_raw_name[index].as_bytes(), 0)
                        }
                    }
                };

                let setter = if is_identity {
                    quote! {
                        #field_vis fn #field_setter(&mut self, index: usize, val: #native_elem) {
                            self.#field_raw_name[index] = val;
                        }
                    }
                } else {
                    quote! {
                        #field_vis fn #field_setter(&mut self, index: usize, val: #native_elem) {
                            <#native_elem as Zorua<#storage_elem>>::write_bits(
                                &val, self.#field_raw_name[index].as_bytes_mut(), 0);
                        }
                    }
                };
                (getter, setter)
            } else {
                // Scalar flat backed field
                let is_identity = quote!(#field_native_type).to_string() == quote!(#field_storage_type).to_string();

                let assertion = gen_fallibility_assertion(f.is_fallible, is_identity, field_name, &quote!(#field_native_type), &quote!(#field_storage_type));

                let getter = if is_identity {
                    quote! {
                        #field_vis fn #field_name(&self) -> #field_native_type {
                            #assertion
                            self.#field_raw_name()
                        }
                    }
                } else if f.is_fallible {
                    quote! {
                        #field_vis fn #field_name(&self) -> Result<#field_native_type, #field_storage_type> {
                            #assertion
                            <#field_native_type as Zorua<#field_storage_type>>::try_read_bits(
                                self.#field_raw_name.as_bytes(), 0)
                        }
                    }
                } else {
                    quote! {
                        #field_vis fn #field_name(&self) -> #field_native_type {
                            #assertion
                            <#field_native_type as Zorua<#field_storage_type>>::read_bits(
                                self.#field_raw_name.as_bytes(), 0)
                        }
                    }
                };

                let setter = if is_identity {
                    quote! {
                        #field_vis fn #field_setter(&mut self, val: #field_native_type) {
                            self.#field_raw_setter(val);
                        }
                    }
                } else {
                    quote! {
                        #field_vis fn #field_setter(&mut self, val: #field_native_type) {
                            <#field_native_type as Zorua<#field_storage_type>>::write_bits(
                                &val, self.#field_raw_name.as_bytes_mut(), 0);
                        }
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
        .filter(|f| f.has_backing_type && deconstruct_array(&f.native_type).is_none())
        .map(|f| {
            let field_attrs = &f.attrs;
            let field_vis = &f.vis;
            let field_raw_name = syn::Ident::new(&format!("{}_raw", f.name), f.name.span());
            let field_raw_setter = prefixed_name("set_", &f.name, "_raw");
            let field_storage_type = &f.storage_type;

            quote! {
                #(#field_attrs)*
                #field_vis fn #field_raw_name(&self) -> #field_storage_type {
                    self.#field_raw_name
                }

                #(#field_attrs)*
                #field_vis fn #field_raw_setter(&mut self, val: #field_storage_type) {
                    self.#field_raw_name = val;
                }
            }
        })
        .collect();

    // Generate bitfield subfield accessors with sequential offset resolution.
    let bitfield_accessors: Vec<_> = fields
        .iter()
        .filter_map(|f| {
            f.bitfield_subfields.as_ref().map(|subfields| {
                let container_name = if f.has_backing_type {
                    syn::Ident::new(&format!("{}_raw", f.name), f.name.span())
                } else {
                    f.name.clone()
                };

                // Resolve offsets: track cumulative position as a token stream.
                // Fields with explicit @offset reset the position; fields without
                // are placed sequentially after the previous field.
                let mut current_offset: proc_macro2::TokenStream = quote! { 0usize };

                subfields
                    .iter()
                    .map(move |sf| {
                        // Determine this field's resolved offset and optional assertion.
                        let (resolved_offset, offset_assertion) =
                            if let Some(ref explicit) = sf.bit_offset {
                                // Explicit @offset — use it and update current.
                                let offset_ts = quote! { #explicit };
                                current_offset = offset_ts.clone();
                                (offset_ts, quote! {})
                            } else {
                                // No @offset — use current position.
                                (current_offset.clone(), quote! {})
                            };

                        // Advance current_offset by this field's BITS.
                        let bits_expr = field_bits_expr(sf);
                        let prev = current_offset.clone();
                        current_offset = quote! { #prev + #bits_expr };

                        generate_subfield_accessor_with_assert(
                            &container_name,
                            sf,
                            &resolved_offset,
                            &offset_assertion,
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect();

    // Generate Zorua<Self> impl from bits annotation
    let bits_impl = if let Some(ref bits_expr) = bits_annotation {
        quote! {
            impl #impl_generics Zorua<#name #ty_generics> for #name #ty_generics #where_clause {
                const BITS: usize = #bits_expr;
                const IS_FALLIBLE: bool = false;

                fn read_bits(src: &[u8], bit_offset: usize) -> Self {
                    // SAFETY: All-zeros is valid for POD types used with bits annotation
                    let mut s: Self = unsafe { core::mem::zeroed() };
                    bits::copy(src, bit_offset, s.as_bytes_mut(), 0, #bits_expr);
                    s
                }

                fn write_bits(&self, dst: &mut [u8], bit_offset: usize) {
                    bits::copy(self.as_bytes(), 0, dst, bit_offset, #bits_expr);
                }
            }
        }
    } else {
        quote! {}
    };

    let output = quote! {
        #(#attrs)*
        #vis struct #name #generics #where_clause {
            #(#field_defs),*
        }

        impl #impl_generics #name #ty_generics #where_clause {
            #(#accessors)*
            #(#raw_accessors)*
            #(#bitfield_accessors)*
        }

        #bits_impl
    };

    Ok(output.into())
}

/// Returns a token stream for the BITS of a subfield (used for sequential offset tracking).
fn field_bits_expr(sf: &BitfieldSubfield) -> proc_macro2::TokenStream {
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
            quote! { #len * <#native_elem as Zorua<#storage_elem>>::BITS }
        }
    } else {
        // Scalar
        if sf.has_backing_type {
            quote! { <#native_ts as Zorua<#storage_ts>>::BITS }
        } else {
            quote! { <#native_ts as Zorua<#native_ts>>::BITS }
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
        is_fallible: sf.is_fallible,
        is_zeroedoption: sf.is_zeroedoption,
        bit_offset: Some(syn::parse2(resolved_offset.clone()).unwrap()),
        stride: sf.stride.clone(),
        offset_assertion: assertion.clone(),
    };
    generate_subfield_accessor(container_name, &resolved)
}

/// Generate a fallibility assertion for a field.
fn gen_fallibility_assertion(
    is_fallible: bool,
    is_identity: bool,
    field_name: &Ident,
    native_type: &proc_macro2::TokenStream,
    storage_type: &proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    if is_identity {
        if is_fallible {
            quote! {
                compile_error!(concat!("Field `", stringify!(#field_name), "` marked #[fallible] but native type matches storage type (infallible)."));
            }
        } else {
            quote! {}
        }
    } else if is_fallible {
        quote! {
            const { assert!(
                <#native_type as Zorua<#storage_type>>::IS_FALLIBLE,
                concat!("Field `", stringify!(#field_name), "` marked #[fallible] but conversion is infallible.")
            ) };
        }
    } else {
        quote! {
            const { assert!(
                !<#native_type as Zorua<#storage_type>>::IS_FALLIBLE,
                concat!("Field `", stringify!(#field_name), "` missing #[fallible] but conversion is fallible.")
            ) };
        }
    }
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

    let assertion = gen_fallibility_assertion(
        sf.is_fallible,
        is_identity,
        sf_name,
        sf_native_type_ts,
        sf_storage_type_ts,
    );

    if sf.has_backing_type {
        // With `as` — has raw accessors
        let sf_raw_name = syn::Ident::new(&format!("{}_raw", sf.name), sf.name.span());
        let sf_raw_setter = prefixed_name("set_", &sf.name, "_raw");

        let getter = if sf.is_fallible {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> Result<#sf_native_type_ts, #sf_storage_type_ts> {
                    #assertion #offset_assertion
                    <#sf_native_type_ts as Zorua<#sf_storage_type_ts>>::try_read_bits(
                        self.#container_name.as_bytes(), #sf_offset)
                }
            }
        } else if is_identity {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> #sf_native_type_ts {
                    #assertion #offset_assertion
                    self.#sf_raw_name()
                }
            }
        } else {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> #sf_native_type_ts {
                    #assertion #offset_assertion
                    <#sf_native_type_ts as Zorua<#sf_storage_type_ts>>::read_bits(
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
                    <#sf_native_type_ts as Zorua<#sf_storage_type_ts>>::write_bits(
                        &val, self.#container_name.as_bytes_mut(), #sf_offset);
                }
            }
        };

        quote! {
            #getter
            #setter

            #(#sf_attrs)*
            #sf_vis fn #sf_raw_name(&self) -> #sf_storage_type_ts {
                <#sf_storage_type_ts as Zorua<#sf_storage_type_ts>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset)
            }

            #(#sf_attrs)*
            #sf_vis fn #sf_raw_setter(&mut self, val: #sf_storage_type_ts) {
                <#sf_storage_type_ts as Zorua<#sf_storage_type_ts>>::write_bits(
                    &val, self.#container_name.as_bytes_mut(), #sf_offset);
            }
        }
    } else {
        // No `as` — identity access, no raw accessors needed
        let getter = if sf.is_fallible {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> Result<#sf_native_type_ts, #sf_native_type_ts> {
                    #assertion #offset_assertion
                    <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::try_read_bits(
                        self.#container_name.as_bytes(), #sf_offset)
                }
            }
        } else {
            quote! {
                #(#sf_attrs)*
                #sf_vis fn #sf_name(&self) -> #sf_native_type_ts {
                    #assertion #offset_assertion
                    <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::read_bits(
                        self.#container_name.as_bytes(), #sf_offset)
                }
            }
        };

        let setter = quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_setter(&mut self, val: #sf_native_type_ts) {
                <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::write_bits(
                    &val, self.#container_name.as_bytes_mut(), #sf_offset);
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
        quote! { <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::BITS }
    } else {
        quote! { <#sf_native_type_ts as Zorua<#sf_storage_type_ts>>::BITS }
    };

    let read_native = if is_identity {
        quote! { <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    } else {
        quote! { <#sf_native_type_ts as Zorua<#sf_storage_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    };

    let write_native = if is_identity {
        quote! { <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::write_bits(&v, self.#container_name.as_bytes_mut(), #sf_offset) }
    } else {
        quote! { <#sf_native_type_ts as Zorua<#sf_storage_type_ts>>::write_bits(&v, self.#container_name.as_bytes_mut(), #sf_offset) }
    };

    let read_raw = if is_identity {
        quote! { <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
    } else {
        quote! { <#sf_storage_type_ts as Zorua<#sf_storage_type_ts>>::read_bits(self.#container_name.as_bytes(), #sf_offset) }
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
                    bits::write_u64(self.#container_name.as_bytes_mut(), #sf_offset, #bits_expr, 0);
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
            <#raw_type as Zorua<#raw_type>>::write_bits(&val, self.#container_name.as_bytes_mut(), #sf_offset);
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
    let sf_setter = prefixed_name("set_", &sf.name, "");
    let sf_offset = sf.bit_offset.as_ref().expect("resolved offset required");
    let offset_assertion = &sf.offset_assertion;

    // Determine stride and element BITS
    let (stride_expr, bits_expr) = if elem_is_identity {
        (
            quote! { <#native_elem as Zorua<#native_elem>>::BITS },
            quote! { <#native_elem as Zorua<#native_elem>>::BITS },
        )
    } else {
        (
            quote! { <#native_elem as Zorua<#storage_elem>>::BITS },
            quote! { <#native_elem as Zorua<#storage_elem>>::BITS },
        )
    };

    let read_elem = if elem_is_identity {
        quote! { <#native_elem as Zorua<#native_elem>>::read_bits(
        self.#container_name.as_bytes(), elem_offset) }
    } else {
        quote! { <#native_elem as Zorua<#storage_elem>>::read_bits(
        self.#container_name.as_bytes(), elem_offset) }
    };

    let write_elem = if elem_is_identity {
        quote! { <#native_elem as Zorua<#native_elem>>::write_bits(
        &v, self.#container_name.as_bytes_mut(), elem_offset) }
    } else {
        quote! { <#native_elem as Zorua<#storage_elem>>::write_bits(
        &v, self.#container_name.as_bytes_mut(), elem_offset) }
    };

    quote! {
        #(#sf_attrs)*
        #sf_vis fn #sf_name(&self, index: usize) -> Option<#native_elem> {
            #offset_assertion
            let stride = #stride_expr;
            let elem_offset = #sf_offset + index * stride;
            if bits::are_zero(self.#container_name.as_bytes(), elem_offset, #bits_expr) {
                None
            } else {
                Some(#read_elem)
            }
        }

        #(#sf_attrs)*
        #sf_vis fn #sf_setter(&mut self, index: usize, val: Option<#native_elem>) {
            let stride = #stride_expr;
            let elem_offset = #sf_offset + index * stride;
            match val {
                None => {
                    bits::zero(self.#container_name.as_bytes_mut(), elem_offset, #bits_expr);
                }
                Some(v) => {
                    #write_elem;
                }
            }
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
    let sf_setter = prefixed_name("set_", &sf.name, "");
    let sf_native_type_ts = &sf.native_type;
    let sf_storage_type_ts = &sf.storage_type;
    let sf_offset = sf.bit_offset.as_ref().expect("resolved offset required");
    let offset_assertion = &sf.offset_assertion;

    let assertion = gen_fallibility_assertion(
        sf.is_fallible,
        elem_is_identity,
        sf_name,
        &quote!(#native_elem),
        &quote!(#storage_elem),
    );

    // Determine stride: explicit `stride` keyword, or from Zorua BITS
    let stride_expr = if let Some(ref stride) = sf.stride {
        quote! { #stride }
    } else if elem_is_identity {
        quote! { <#native_elem as Zorua<#native_elem>>::BITS }
    } else {
        quote! { <#native_elem as Zorua<#storage_elem>>::BITS }
    };

    let getter = if elem_is_identity {
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_name(&self, index: usize) -> #native_elem {
                #assertion #offset_assertion
                let stride = #stride_expr;
                <#native_elem as Zorua<#native_elem>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset + index * stride)
            }
        }
    } else if sf.is_fallible {
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_name(&self, index: usize) -> Result<#native_elem, #storage_elem> {
                #assertion #offset_assertion
                let stride = #stride_expr;
                <#native_elem as Zorua<#storage_elem>>::try_read_bits(
                    self.#container_name.as_bytes(), #sf_offset + index * stride)
            }
        }
    } else {
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_name(&self, index: usize) -> #native_elem {
                #assertion #offset_assertion
                let stride = #stride_expr;
                <#native_elem as Zorua<#storage_elem>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset + index * stride)
            }
        }
    };

    let setter = if elem_is_identity {
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_setter(&mut self, index: usize, val: #native_elem) {
                let stride = #stride_expr;
                <#native_elem as Zorua<#native_elem>>::write_bits(
                    &val, self.#container_name.as_bytes_mut(), #sf_offset + index * stride);
            }
        }
    } else {
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_setter(&mut self, index: usize, val: #native_elem) {
                let stride = #stride_expr;
                <#native_elem as Zorua<#storage_elem>>::write_bits(
                    &val, self.#container_name.as_bytes_mut(), #sf_offset + index * stride);
            }
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
                <#sf_storage_type_ts as Zorua<#sf_storage_type_ts>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset)
            }
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_setter(&mut self, val: #sf_storage_type_ts) {
                <#sf_storage_type_ts as Zorua<#sf_storage_type_ts>>::write_bits(
                    &val, self.#container_name.as_bytes_mut(), #sf_offset);
            }
        }
    } else {
        let sf_raw_name = syn::Ident::new(&format!("{}_raw", sf.name), sf.name.span());
        let sf_raw_setter = prefixed_name("set_", &sf.name, "_raw");
        quote! {
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_name(&self) -> #sf_native_type_ts {
                <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::read_bits(
                    self.#container_name.as_bytes(), #sf_offset)
            }
            #(#sf_attrs)*
            #sf_vis fn #sf_raw_setter(&mut self, val: #sf_native_type_ts) {
                <#sf_native_type_ts as Zorua<#sf_native_type_ts>>::write_bits(
                    &val, self.#container_name.as_bytes_mut(), #sf_offset);
            }
        }
    };

    quote! {
        #getter
        #setter
        #raw_accessors
    }
}
