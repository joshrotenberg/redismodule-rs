//! Private parser for the metadata grammar accepted by the public macros.
//!
//! This intentionally supports the existing JSON-like syntax, not arbitrary
//! Rust expressions or a general serialization format. Keep compatibility tests
//! against the old parser when extending this grammar.

use proc_macro2::Span;
use syn::{
    Ident, LitBool, LitInt, LitStr, Token, braced, bracketed, parenthesized,
    parse::{ParseBuffer, ParseStream},
};

pub(crate) trait MetadataValue: Sized {
    fn parse_metadata(input: ParseStream) -> syn::Result<Self>;

    fn missing(field: &str, span: Span) -> syn::Result<Self> {
        Err(syn::Error::new(span, format!("missing field `{field}`")))
    }
}

pub(crate) fn identifier(input: ParseStream) -> syn::Result<(String, Span)> {
    if input.peek(LitStr) {
        let name: LitStr = input.parse()?;
        Ok((name.value(), name.span()))
    } else {
        let name: Ident = input.parse()?;
        Ok((name.to_string(), name.span()))
    }
}

fn is_ident(input: ParseStream, name: &str) -> bool {
    input
        .fork()
        .parse::<Ident>()
        .is_ok_and(|ident| ident == name)
}

pub(crate) fn fields<'a>(input: ParseStream<'a>, name: &str) -> syn::Result<ParseBuffer<'a>> {
    if is_ident(input, name) {
        input.parse::<Ident>()?;
    }
    let content;
    braced!(content in input);
    Ok(content)
}

pub(crate) fn variant(input: ParseStream, name: &str) -> syn::Result<(String, Span)> {
    if is_ident(input, name) {
        input.parse::<Ident>()?;
        input.parse::<Token![::]>()?;
    } else if input.peek(Token![::]) {
        input.parse::<Token![::]>()?;
    }
    identifier(input)
}

pub(crate) fn payload<T: MetadataValue>(input: ParseStream) -> syn::Result<T> {
    let content;
    parenthesized!(content in input);
    let value = T::parse_metadata(&content)?;
    if !content.is_empty() {
        return Err(content.error("unexpected tokens after metadata value"));
    }
    Ok(value)
}

impl MetadataValue for String {
    fn parse_metadata(input: ParseStream) -> syn::Result<Self> {
        Ok(input.parse::<LitStr>()?.value())
    }
}

impl MetadataValue for bool {
    fn parse_metadata(input: ParseStream) -> syn::Result<Self> {
        Ok(input.parse::<LitBool>()?.value)
    }
}

macro_rules! integer {
    ($ty:ty, $signed:literal) => {
        impl MetadataValue for $ty {
            fn parse_metadata(input: ParseStream) -> syn::Result<Self> {
                let negative = input.parse::<Option<Token![-]>>()?;
                let value: LitInt = input.parse()?;
                if !value.suffix().is_empty() && value.suffix() != stringify!($ty) {
                    return Err(syn::Error::new(value.span(), concat!("expected ", stringify!($ty))));
                }
                // Parse magnitude first to preserve the legacy signed-minimum
                // rejection, as well as its radix, separator and suffix rules.
                let magnitude = value.base10_parse::<$ty>()?;
                if let Some(sign) = negative {
                    if !$signed {
                        return Err(syn::Error::new(sign.span, "integer is unsigned"));
                    }
                    magnitude.checked_neg().ok_or_else(|| {
                        syn::Error::new(value.span(), "integer out of range")
                    })
                } else {
                    Ok(magnitude)
                }
            }
        }
    };
}

integer!(i32, true);
integer!(i64, true);
integer!(u32, false);

impl<T: MetadataValue> MetadataValue for Vec<T> {
    fn parse_metadata(input: ParseStream) -> syn::Result<Self> {
        let content;
        bracketed!(content in input);
        let mut values = Vec::new();
        while !content.is_empty() {
            values.push(T::parse_metadata(&content)?);
            if !content.is_empty() {
                content.parse::<Token![,]>()?;
            }
        }
        Ok(values)
    }
}

impl<T: MetadataValue> MetadataValue for Option<T> {
    fn parse_metadata(input: ParseStream) -> syn::Result<Self> {
        // Options accept bare values, Some/None, Option::Some/None and
        // ::Some/None. A leading ::Option was not part of the old grammar.
        if input.peek(Token![::])
            || is_ident(input, "Option")
            || is_ident(input, "Some")
            || is_ident(input, "None")
        {
            if input.peek(Token![::]) {
                input.parse::<Token![::]>()?;
            } else if is_ident(input, "Option") {
                input.parse::<Ident>()?;
                input.parse::<Token![::]>()?;
            }
            let name: Ident = input.parse()?;
            match name.to_string().as_str() {
                "Some" => payload(input).map(Some),
                "None" => Ok(None),
                _ => Err(syn::Error::new(name.span(), "expected Some or None")),
            }
        } else {
            T::parse_metadata(input).map(Some)
        }
    }

    fn missing(_field: &str, _span: Span) -> syn::Result<Self> {
        Ok(None)
    }
}

macro_rules! metadata_struct {
    ($(#[$attr:meta])* $vis:vis struct $name:ident {
        $($field_vis:vis $field:ident: $ty:ty),* $(,)?
    }) => {
        $(#[$attr])*
        $vis struct $name { $($field_vis $field: $ty),* }

        impl crate::metadata::MetadataValue for $name {
            fn parse_metadata(input: syn::parse::ParseStream) -> syn::Result<Self> {
                let content = crate::metadata::fields(input, stringify!($name))?;
                $(let mut $field: Option<$ty> = None;)*
                while !content.is_empty() {
                    let (field, span) = crate::metadata::identifier(&content)?;
                    content.parse::<syn::Token![:]>()?;
                    match field.as_str() {
                        $(stringify!($field) => {
                            if $field.is_some() {
                                return Err(syn::Error::new(span, concat!("duplicate field `", stringify!($field), "`")));
                            }
                            $field = Some(<$ty as crate::metadata::MetadataValue>::parse_metadata(&content)?);
                        },)*
                        _ => return Err(syn::Error::new(span, format!("unknown field `{field}`"))),
                    }
                    if !content.is_empty() { content.parse::<syn::Token![,]>()?; }
                }
                Ok(Self {
                    $($field: match $field {
                        Some(value) => value,
                        None => <$ty as crate::metadata::MetadataValue>::missing(stringify!($field), content.span())?,
                    }),*
                })
            }
        }
    };
}

macro_rules! metadata_enum {
    ($(#[$attr:meta])* $vis:vis enum $name:ident {
        $($(#[$variant_attr:meta])* $variant:ident $(($ty:ty))?),* $(,)?
    }) => {
        $(#[$attr])*
        $vis enum $name { $($(#[$variant_attr])* $variant $(($ty))?),* }
        crate::metadata::enum_parser!($name { $($variant $(($ty))?),* });
    };
}

// Separate from the declaration macro so shared ACL types keep their existing
// public definition and serialization behavior in the common crate.
macro_rules! enum_parser {
    ($name:ident { $($variant:ident $(($ty:ty))?),* $(,)? }) => {
        impl crate::metadata::MetadataValue for $name {
            fn parse_metadata(input: syn::parse::ParseStream) -> syn::Result<Self> {
                let (variant, span) = crate::metadata::variant(input, stringify!($name))?;
                match variant.as_str() {
                    $(stringify!($variant) => Ok(Self::$variant $( (crate::metadata::payload::<$ty>(input)?) )? ),)*
                    _ => Err(syn::Error::new(span, format!(
                        "unknown variant `{variant}`, expected one of {}",
                        [$(stringify!($variant)),*].join(", ")
                    ))),
                }
            }
        }
    };
}

pub(crate) use {enum_parser, metadata_enum, metadata_struct};
