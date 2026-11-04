mod api_versions;
mod command_list;

use api_versions::{API_OLDEST_VERSION, API_VERSION_MAPPING, get_feature_flags};
use proc_macro::TokenStream;
use quote::quote;
use syn::Ident;
use syn::ItemFn;
use syn::parse::{Parse, ParseStream, Result};
use syn::punctuated::Punctuated;
use syn::token::{self, Paren, RArrow};
use syn::{self, ReturnType, Token, Type, TypeTuple, bracketed, parse_macro_input};

#[derive(Debug)]
struct Args {
    requested_apis: Vec<Ident>,
    function: ItemFn,
}

impl Parse for Args {
    fn parse(input: ParseStream) -> Result<Self> {
        let content;
        let _paren_token: token::Bracket = bracketed!(content in input);
        let vars: Punctuated<Ident, Token![,]> =
            content.parse_terminated(Ident::parse, Token![,])?;
        input.parse::<Token![,]>()?;
        let function: ItemFn = input.parse()?;
        Ok(Args {
            requested_apis: vars.into_iter().collect(),
            function,
        })
    }
}

/// Declares the native APIs required by an SDK-internal wrapper.
///
/// When the selected SDK compatibility floor includes every listed API, the
/// wrapper keeps its return type and extracts required API pointers. At an
/// older floor its return type becomes `crate::apierror::APIResult<T>`; missing
/// pointers return an error before the wrapper body runs.
///
/// This macro expands against the SDK's `crate::raw`, `crate::apierror`,
/// `crate::__required_api!` and compatibility features. It is not a standalone
/// module registration interface.
///
/// # Panics
///
/// At a floor that requires an API, the SDK's required-API check panics if the
/// server has no binding for it. At an older floor, a missing binding returns
/// an error instead.
///
/// # Input syntax
///
/// This compiling example constructs the macro's input tokens, not a native
/// registration or a standalone expansion. Callback wrappers use the current
/// `FnOnce(&EventContext) + 'static` contract. The SDK's
/// `EventContext::add_post_notification_job` rustdoc checks use of the actual
/// public method; the wrapper body below is only a placeholder.
///
/// ```
/// use quote::quote;
/// let wrapper: syn::ItemFn = syn::parse_quote! {
///     pub fn add_post_notification_job<F: FnOnce(&EventContext) + 'static>(
///         &self, callback: F,
///     ) -> Status {
///         install_job(self, callback)
///     }
/// };
/// let input = quote! { [RedisModule_AddPostNotificationJob], #wrapper };
/// assert!(input.to_string().contains("RedisModule_AddPostNotificationJob"));
/// ```
#[proc_macro]
pub fn api(item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(item as Args);
    let minimum_require_version =
        args.requested_apis
            .iter()
            .fold(*API_OLDEST_VERSION, |min_api_version, item| {
                // if we do not have a version mapping, we assume the API exists and return the minimum version.
                let api_version = API_VERSION_MAPPING
                    .get(&item.to_string())
                    .copied()
                    .unwrap_or(*API_OLDEST_VERSION);
                api_version.max(min_api_version)
            });

    let requested_apis = args.requested_apis;
    let requested_apis_str: Vec<String> = requested_apis.iter().map(|e| e.to_string()).collect();

    let original_func = args.function;
    let original_func_attr = original_func.attrs;
    let original_func_code = original_func.block;
    let original_func_sig = original_func.sig;
    let original_func_vis = original_func.vis;

    let inner_return_return_type = match original_func_sig.output.clone() {
        ReturnType::Default => Box::new(Type::Tuple(TypeTuple {
            paren_token: Paren::default(),
            elems: Punctuated::new(),
        })),
        ReturnType::Type(_, t) => t,
    };
    let new_return_return_type = Type::Path(
        syn::parse(
            quote!(
                crate::apierror::APIResult<#inner_return_return_type>
            )
            .into(),
        )
        .unwrap(),
    );

    let mut new_func_sig = original_func_sig.clone();
    new_func_sig.output = ReturnType::Type(RArrow::default(), Box::new(new_return_return_type));

    let old_ver_func = quote!(
        #(#original_func_attr)*
        #original_func_vis #new_func_sig {
            #(
                #[allow(non_snake_case)]
                let #requested_apis = unsafe{crate::raw::#requested_apis.ok_or(concat!("ERR ", #requested_apis_str, " does not exists"))?};
            )*
            let __callback__ = move || -> #inner_return_return_type {
                #original_func_code
            };
            Ok(__callback__())
        }
    );

    let new_ver_func = quote!(
        #(#original_func_attr)*
        #original_func_vis #original_func_sig {
            #(
                #[allow(non_snake_case)]
                let #requested_apis = unsafe{crate::__required_api!(#requested_apis)};
            )*
            let __callback__ = move || -> #inner_return_return_type {
                #original_func_code
            };
            __callback__()
        }
    );

    let (all_lower_features, all_upper_features) = get_feature_flags(minimum_require_version);

    let r#gen = quote! {
        cfg_if::cfg_if! {
            if #[cfg(any(#(#all_lower_features, )*))] {
                #old_ver_func
            } else if #[cfg(any(#(#all_upper_features, )*))] {
                #new_ver_func
            } else {
                compile_error!("min-redis-compatibility-version is not set correctly")
            }
        }
    };
    r#gen.into()
}

#[doc(hidden)]
#[proc_macro]
pub fn __register_command_list(input: TokenStream) -> TokenStream {
    command_list::expand(input)
}
