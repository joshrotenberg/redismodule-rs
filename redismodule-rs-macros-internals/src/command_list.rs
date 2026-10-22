//! Normalize mixed command lists without recursive macro expansion per entry.
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Expr, Token, bracketed, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

struct CommandList {
    sdk: TokenStream2,
    ctx: Expr,
    entries: Punctuated<Expr, Token![,]>,
}

impl Parse for CommandList {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let sdk;
        parenthesized!(sdk in input);
        let sdk = sdk.parse()?;
        input.parse::<Token![,]>()?;
        let ctx = input.parse()?;
        input.parse::<Token![,]>()?;
        let entries;
        bracketed!(entries in input);
        let entries = entries.parse_terminated(Expr::parse, Token![,])?;
        Ok(Self { sdk, ctx, entries })
    }
}

pub fn expand(input: TokenStream) -> TokenStream {
    let CommandList { sdk, ctx, entries } = syn::parse_macro_input!(input as CommandList);
    let mut registrations = TokenStream2::new();
    for entry in entries {
        if let Expr::Array(array) = entry {
            if !(7..=8).contains(&array.elems.len()) {
                return syn::Error::new_spanned(
                    array,
                    "a legacy command entry has seven or eight fields",
                )
                .into_compile_error()
                .into();
            }
            let fields = array.elems;
            registrations.extend(quote! { #sdk::redis_command!(#ctx, #fields); });
        } else {
            registrations.extend(quote! {
                if let Err(error) = (#entry).register(#ctx) {
                    (#ctx).log_warning(&error.to_string());
                    return #sdk::Status::Err as i32;
                }
            });
        }
    }
    registrations.into()
}
