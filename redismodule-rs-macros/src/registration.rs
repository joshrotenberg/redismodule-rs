use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{ItemFn, Path};

pub(crate) fn attributes(slice: Path) -> TokenStream2 {
    // The hint must follow distributed_slice so linkme consumes it before
    // rustc tries to resolve it as a separate attribute.
    quote! {
        #[::redis_module::__private::linkme::distributed_slice(#slice)]
        #[linkme(crate = ::redis_module::__private::linkme)]
    }
}

pub(crate) fn handler(item: TokenStream, slice: Path) -> TokenStream {
    let function: ItemFn = match syn::parse(item) {
        Ok(function) => function,
        Err(error) => return error.to_compile_error().into(),
    };
    let attributes = attributes(slice);
    quote! {
        #attributes
        #function
    }
    .into()
}
