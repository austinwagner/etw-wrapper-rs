//! Generates strongly typed ETW wrappers from manifest files.
//!
//! When invoked with only a path, [`gen_etw_wrapper!`] names each wrapper after the provider
//! symbol by converting it to `PascalCaseSymbolLogger`.
//! ```text
//! gen_etw_wrapper!("path/to/manifest.man");
//! ```
//!
//! Wrapper names can be overridden by mapping the provider symbol to the desired name. If the
//! provider symbol is not a valid Rust identifier, it must be provided as a string literal.
//! ```text
//! gen_etw_wrapper!("path/to/manifest.man", PROVIDER_WIDGETSERVICE -> WidgetLogger);
//! ```

mod eventman;
mod model;

use std::collections::HashMap;
use std::path::PathBuf;

use crate::model::{AnsiEncoding, Count, Event, Length, Provider, WinType};
use convert_case::ccase;
use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, LitStr, Token};
use unicode_ident::{is_xid_continue, is_xid_start};

/// The parsed macro input, including the manifest path and optional wrapper name overrides.
struct WrapperArgs {
    path: LitStr,
    overrides: Vec<NameOverride>,
}

/// A `SYMBOL -> NewName` override. `key` is matched against a provider's
/// manifest `symbol`, `name` is the struct name to generate instead of the default.
struct NameOverride {
    key: String,
    /// Span of the key token, so an unmatched override can be reported against the symbol
    /// the caller wrote rather than the whole macro invocation.
    key_span: Span,
    name: Ident,
}

/// Paths used by generated code.
struct CodegenContext {
    runtime: TokenStream2,
}

impl CodegenContext {
    fn resolve() -> anyhow::Result<Self> {
        let runtime = match crate_name("etw-wrapper")? {
            // Use the stable self-alias exported by etw-wrapper. Unlike `crate`, this also works
            // when expansion occurs in one of that package's integration tests or doctests.
            FoundCrate::Itself => quote!(::etw_wrapper),
            FoundCrate::Name(name) => {
                let name = Ident::new(&name, Span::call_site());
                quote!(::#name)
            }
        };
        Ok(Self { runtime })
    }

    #[cfg(test)]
    fn canonical() -> Self {
        Self {
            runtime: quote!(::etw_wrapper),
        }
    }
}

impl Parse for WrapperArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let path: LitStr = input.parse()?;
        let mut overrides = Vec::new();

        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            // Tolerate a trailing comma
            if input.is_empty() {
                break;
            }

            let (key, key_span) = if input.peek(LitStr) {
                let lit: LitStr = input.parse()?;
                (lit.value(), lit.span())
            } else {
                let ident: Ident = input.parse()?;
                (ident.to_string(), ident.span())
            };
            input.parse::<Token![->]>()?;
            let name: Ident = input.parse()?;
            overrides.push(NameOverride {
                key,
                key_span,
                name,
            });
        }

        Ok(WrapperArgs { path, overrides })
    }
}

#[proc_macro]
pub fn gen_etw_wrapper(input: TokenStream) -> TokenStream {
    let args = syn::parse_macro_input!(input as WrapperArgs);
    match impl_gen_etw_wrapper(&args) {
        Ok(ts) => ts.into(),
        // A syn::Error carries a span, so let it point at the offending tokens. Everything else
        // originates in the manifest file and can only be reported against the whole invocation,
        // with "{:#}" flattening anyhow's context chain into the message.
        Err(e) => match e.downcast::<syn::Error>() {
            Ok(e) => e.to_compile_error().into(),
            Err(e) => {
                let msg = format!("{e:#}");
                quote! { ::core::compile_error!(#msg); }.into()
            }
        },
    }
}

fn impl_gen_etw_wrapper(args: &WrapperArgs) -> anyhow::Result<TokenStream2> {
    let path = resolve_path(&args.path.value());
    let manifest = model::load(&path)?;
    let codegen = CodegenContext::resolve()?;

    // Map each override to its provider symbol
    let mut overrides: HashMap<&str, &Ident> = HashMap::new();
    for o in &args.overrides {
        if !manifest.providers.iter().any(|p| p.symbol == o.key) {
            return Err(syn::Error::new(
                o.key_span,
                format!(
                    "name override {:?} does not match any provider symbol in the manifest",
                    o.key
                ),
            )
            .into());
        }
        overrides.insert(o.key.as_str(), &o.name);
    }

    let providers = manifest
        .providers
        .iter()
        .map(|p| gen_provider(p, overrides.get(p.symbol.as_str()).copied(), &codegen))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Force a rebuild when the manifest file changes by using include_bytes so
    // Cargo tracks changes to the file
    let abs = path.to_string_lossy().into_owned();

    Ok(quote! {
        const _: &[u8] = ::core::include_bytes!(#abs);
        #(#providers)*
    })
}

/// Resolves the manifest path relative to the caller's crate root.
fn resolve_path(raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p;
    }
    match std::env::var_os("CARGO_MANIFEST_DIR") {
        Some(dir) => PathBuf::from(dir).join(p),
        None => p,
    }
}

fn gen_provider(
    p: &Provider,
    name_override: Option<&Ident>,
    codegen: &CodegenContext,
) -> anyhow::Result<TokenStream2> {
    // The default name is the PascalCase provider symbol with a Logger suffix, an
    // explicit override replaces it wholesale
    let struct_name = match name_override {
        Some(name) => name.clone(),
        None => format_ident!("{}Logger", ccase!(pascal, &p.symbol)),
    };
    let guid = p.guid;

    // Emit one public method per event and one shared backing helper per distinct
    // WinType parameter signature, keyed by the generated helper name
    let mut backing: std::collections::BTreeMap<String, TokenStream2> = Default::default();
    let mut methods: Vec<TokenStream2> = Vec::new();
    let mut method_idents: HashMap<String, String> = HashMap::new();
    for ev in &p.events {
        let specs = ev
            .params
            .iter()
            .map(|t| classify(t, codegen))
            .collect::<Vec<_>>();
        let helper = backing_name(&specs);
        backing
            .entry(helper.clone())
            .or_insert_with(|| gen_backing(&helper, &specs, codegen));
        let (method_ident, method) = gen_event_method(ev, &specs, &helper, p, codegen)?;
        if let Some(prev) = method_idents.insert(method_ident.clone(), ev.symbol.clone()) {
            anyhow::bail!(
                "event name {} collides with {} in provider {} (both transform to: {})",
                ev.symbol,
                prev,
                p.symbol,
                method_ident
            );
        }
        methods.push(method);
    }

    let helpers = backing.values();
    let runtime = &codegen.runtime;

    Ok(quote! {
        pub struct #struct_name {
            ctx: #runtime::EtwLogger,
        }

        impl #struct_name {
            /// Registers the provider with ETW.
            ///
            /// The provider is automatically unregistered when dropped.
            pub fn register() -> #runtime::Result<Self> {
                let guid = #runtime::GUID::from_u128(#guid);
                let ctx = #runtime::EtwLogger::register(&guid)?;
                #runtime::Result::Ok(Self { ctx })
            }

            #(#methods)*

            #(#helpers)*
        }
    })
}

/// One template field lowered for codegen: an optional public parameter, an optional
/// statement to run before the helper call (length derivation or string truncation),
/// the identifier handed to the backing helper for this field, and how the field is named
/// when a message placeholder (`%N`) refers to it.
struct FieldPlan {
    param: Option<(Ident, TokenStream2)>,
    temp: Option<TokenStream2>,
    call: Ident,
    doc_ref: String,
}

/// Whether a manifest field is emitted from a caller-provided value or derived from another field.
#[derive(Clone)]
enum FieldRole {
    Value(ValuePlan),
    Derived(DerivedPlan),
}

/// The independent dimensions needed to expose and serialize a caller-provided value.
#[derive(Clone, Copy)]
struct ValuePlan {
    kind: ValueKind,
    cardinality: Cardinality,
}

#[derive(Clone, Copy)]
enum ValueKind {
    Scalar,
    Boolean,
    String {
        encoding: StringEncoding,
        length: ElementLength,
    },
    Binary {
        length: ElementLength,
    },
    Sid,
}

#[derive(Clone, Copy)]
enum StringEncoding {
    Unicode,
    ProviderAnsi,
    Utf8,
}

#[derive(Clone, Copy)]
enum ElementLength {
    Implicit,
    Fixed(usize),
    FieldRef(usize),
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Cardinality {
    Single,
    Fixed(usize),
    Dynamic,
}

impl Cardinality {
    fn from_count(count: &Count) -> Self {
        match count {
            Count::Single => Self::Single,
            Count::Constant(count) => Self::Fixed(*count as usize),
            Count::FieldRef(_) => Self::Dynamic,
        }
    }

    fn is_array(self) -> bool {
        self != Self::Single
    }
}

#[derive(Clone)]
enum DerivedPlan {
    BinaryLength { from: usize },
    ArrayCount { from: Vec<usize> },
}

/// Returns whether a `WinType` is an integer type that a `win:Binary length="..."` reference may
/// target.
fn is_length_int(w: &WinType) -> bool {
    matches!(
        w,
        WinType::UInt8 | WinType::UInt16 | WinType::UInt32 | WinType::HexInt32
    )
}

fn array_param_ty(element: TokenStream2, cardinality: Cardinality) -> anyhow::Result<TokenStream2> {
    match cardinality {
        Cardinality::Fixed(count) => {
            let n = count;
            Ok(quote!(&[#element; #n]))
        }
        Cardinality::Dynamic => Ok(quote!(&[#element])),
        Cardinality::Single => anyhow::bail!("internal error: array parameter has no count"),
    }
}

fn resolve_prior_length_field(
    index_by_name: &HashMap<&str, usize>,
    ev: &Event,
    p: &Provider,
    field_index: usize,
    name: &str,
    subject: &str,
) -> anyhow::Result<usize> {
    let field = &ev.params[field_index];
    let index = *index_by_name.get(name).ok_or_else(|| {
        anyhow::anyhow!(
            "{subject} {} in event {} of provider {} references unknown length field {:?}",
            field.name,
            ev.symbol,
            p.symbol,
            name
        )
    })?;
    if index >= field_index {
        anyhow::bail!(
            "{subject} {} in event {} of provider {} references length field {:?}, which must appear before it",
            field.name,
            ev.symbol,
            p.symbol,
            name
        );
    }
    if !is_length_int(&ev.params[index].win_type) || ev.params[index].count != Count::Single {
        anyhow::bail!(
            "length field {:?} for {subject} {} in event {} of provider {} must be a scalar win:UInt8, win:UInt16, win:UInt32, or win:HexInt32",
            name,
            field.name,
            ev.symbol,
            p.symbol
        );
    }
    Ok(index)
}

fn build_value_plan(
    index_by_name: &HashMap<&str, usize>,
    ev: &Event,
    p: &Provider,
    field_index: usize,
) -> anyhow::Result<ValuePlan> {
    let field = &ev.params[field_index];
    let cardinality = Cardinality::from_count(&field.count);
    let string_length = |length: &Length| -> anyhow::Result<ElementLength> {
        match length {
            Length::Implicit => Ok(ElementLength::Implicit),
            Length::Constant(length) => Ok(ElementLength::Fixed(*length as usize)),
            // Existing scalar-string behavior treats a referenced length as NUL-terminated. Arrays
            // need the resolved field because each encoded element must have exactly that length.
            Length::FieldRef(_) if cardinality == Cardinality::Single => {
                Ok(ElementLength::Implicit)
            }
            Length::FieldRef(name) => Ok(ElementLength::FieldRef(resolve_prior_length_field(
                index_by_name,
                ev,
                p,
                field_index,
                name,
                "string array field",
            )?)),
        }
    };

    let kind = if scalar_frag(&field.win_type, false).is_some() {
        ValueKind::Scalar
    } else {
        match &field.win_type {
            WinType::Boolean => ValueKind::Boolean,
            WinType::UnicodeString(length) => ValueKind::String {
                encoding: StringEncoding::Unicode,
                length: string_length(length)?,
            },
            WinType::AnsiString(length, encoding) => ValueKind::String {
                encoding: match encoding {
                    AnsiEncoding::ProviderAnsi => StringEncoding::ProviderAnsi,
                    AnsiEncoding::Utf8 => StringEncoding::Utf8,
                },
                length: string_length(length)?,
            },
            WinType::Binary(length) => ValueKind::Binary {
                length: match length {
                    Length::Implicit if cardinality.is_array() => {
                        anyhow::bail!(
                            "binary array field {} in event {} of provider {} must declare a length",
                            field.name,
                            ev.symbol,
                            p.symbol
                        )
                    }
                    Length::Implicit => ElementLength::Implicit,
                    Length::Constant(length) => ElementLength::Fixed(*length as usize),
                    Length::FieldRef(name) => ElementLength::FieldRef(resolve_prior_length_field(
                        index_by_name,
                        ev,
                        p,
                        field_index,
                        name,
                        "binary field",
                    )?),
                },
            },
            WinType::Sid => ValueKind::Sid,
            _ => unreachable!("all scalar types returned above"),
        }
    };

    Ok(ValuePlan { kind, cardinality })
}

fn direct_field_plan(id: Ident, ty: TokenStream2) -> FieldPlan {
    let doc_ref = id.to_string();
    FieldPlan {
        param: Some((id.clone(), ty)),
        temp: None,
        call: id,
        doc_ref,
    }
}

fn plan_string_value(
    field_index: usize,
    value: ValuePlan,
    id: Ident,
    idents: &[Ident],
    specs: &[FieldSpec],
    codegen: &CodegenContext,
) -> anyhow::Result<FieldPlan> {
    let runtime = &codegen.runtime;
    let ValueKind::String { encoding, length } = value.kind else {
        unreachable!("string planner requires a string value plan");
    };

    if value.cardinality == Cardinality::Single {
        return Ok(match encoding {
            StringEncoding::Unicode => {
                let tmp = match length {
                    ElementLength::Fixed(_) => format_ident!("__f{}", field_index),
                    ElementLength::Implicit | ElementLength::FieldRef(_) => {
                        format_ident!("__s{}", field_index)
                    }
                };
                let encode = match length {
                    ElementLength::Fixed(length) => {
                        quote!(#runtime::field::to_u16cstring_fixed_len(#id, #length))
                    }
                    ElementLength::Implicit | ElementLength::FieldRef(_) => {
                        quote!(#runtime::field::to_u16cstring(#id))
                    }
                };
                let temp = quote! {
                    let #tmp: &[u16] = &#encode;
                };
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id, quote!(&str))),
                    temp: Some(temp),
                    call: tmp,
                    doc_ref,
                }
            }
            StringEncoding::ProviderAnsi => match length {
                ElementLength::Fixed(length) => direct_field_plan(id, quote!(&[u8; #length])),
                ElementLength::Implicit | ElementLength::FieldRef(_) => {
                    direct_field_plan(id, specs[field_index].rust_ty.clone())
                }
            },
            StringEncoding::Utf8 => {
                let tmp = match length {
                    ElementLength::Fixed(_) => format_ident!("__af{}", field_index),
                    ElementLength::Implicit | ElementLength::FieldRef(_) => {
                        format_ident!("__a{}", field_index)
                    }
                };
                let encode = match length {
                    ElementLength::Fixed(length) => {
                        quote!(#runtime::field::to_cstring_fixed_len(#id, #length))
                    }
                    ElementLength::Implicit | ElementLength::FieldRef(_) => {
                        quote!(#runtime::field::to_cstring(#id))
                    }
                };
                let temp = quote! {
                    let #tmp: &[u8] = &#encode;
                };
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id, quote!(&str))),
                    temp: Some(temp),
                    call: tmp,
                    doc_ref,
                }
            }
        });
    }

    match encoding {
        StringEncoding::Unicode => {
            let ty = array_param_ty(quote!(&str), value.cardinality)?;
            let storage = format_ident!("__ua{}_storage", field_index);
            let tmp = format_ident!("__ua{}", field_index);
            let encode = match length {
                ElementLength::Implicit => quote!(#runtime::field::to_u16cstring(value)),
                ElementLength::Fixed(length) => {
                    quote!(#runtime::field::to_u16cstring_fixed_len(value, #length))
                }
                ElementLength::FieldRef(from) => {
                    let length = &idents[from];
                    quote!(#runtime::field::to_u16cstring_fixed_len(value, #length as usize))
                }
            };
            let temp = quote! {
                let mut #storage = ::std::vec::Vec::<u16>::new();
                for value in #id {
                    let encoded = #encode;
                    #storage.extend_from_slice(&encoded);
                }
                let #tmp: &[u16] = &#storage;
            };
            let doc_ref = id.to_string();
            Ok(FieldPlan {
                param: Some((id, ty)),
                temp: Some(temp),
                call: tmp,
                doc_ref,
            })
        }
        StringEncoding::ProviderAnsi => {
            let element_ty = match length {
                ElementLength::Fixed(length) => quote!([u8; #length]),
                ElementLength::Implicit | ElementLength::FieldRef(_) => quote!(&[u8]),
            };
            let ty = array_param_ty(element_ty, value.cardinality)?;
            let storage = format_ident!("__aa{}_storage", field_index);
            let tmp = format_ident!("__aa{}", field_index);
            let validate_len = match length {
                ElementLength::FieldRef(from) => {
                    let length = &idents[from];
                    quote! {
                        #runtime::field::ensure_len(value.len(), #length as usize)?;
                    }
                }
                ElementLength::Implicit | ElementLength::Fixed(_) => TokenStream2::new(),
            };
            let temp = quote! {
                let mut #storage = ::std::vec::Vec::<u8>::new();
                for value in #id {
                    #validate_len
                    assert_eq!(value.last(), ::core::option::Option::Some(&0));
                    #storage.extend_from_slice(value);
                }
                let #tmp: &[u8] = &#storage;
            };
            let doc_ref = id.to_string();
            Ok(FieldPlan {
                param: Some((id, ty)),
                temp: Some(temp),
                call: tmp,
                doc_ref,
            })
        }
        StringEncoding::Utf8 => {
            let ty = array_param_ty(quote!(&str), value.cardinality)?;
            let storage = format_ident!("__u8a{}_storage", field_index);
            let tmp = format_ident!("__u8a{}", field_index);
            let encode = match length {
                ElementLength::Implicit => quote!(#runtime::field::to_cstring(value)),
                ElementLength::Fixed(length) => {
                    quote!(#runtime::field::to_cstring_fixed_len(value, #length))
                }
                ElementLength::FieldRef(from) => {
                    let length = &idents[from];
                    quote!(#runtime::field::to_cstring_fixed_len(value, #length as usize))
                }
            };
            let temp = quote! {
                let mut #storage = ::std::vec::Vec::<u8>::new();
                for value in #id {
                    let encoded = #encode;
                    #storage.extend_from_slice(&encoded);
                }
                let #tmp: &[u8] = &#storage;
            };
            let doc_ref = id.to_string();
            Ok(FieldPlan {
                param: Some((id, ty)),
                temp: Some(temp),
                call: tmp,
                doc_ref,
            })
        }
    }
}

fn plan_value_field(
    field_index: usize,
    value: ValuePlan,
    id: Ident,
    idents: &[Ident],
    ev: &Event,
    specs: &[FieldSpec],
    codegen: &CodegenContext,
) -> anyhow::Result<FieldPlan> {
    let runtime = &codegen.runtime;

    match value.kind {
        ValueKind::Scalar => {
            if value.cardinality == Cardinality::Single {
                Ok(direct_field_plan(id, specs[field_index].rust_ty.clone()))
            } else {
                let element_ty = scalar_rust_ty(&ev.params[field_index].win_type, codegen)
                    .expect("scalar value plan must have a scalar type");
                let ty = array_param_ty(element_ty, value.cardinality)?;
                Ok(direct_field_plan(id, ty))
            }
        }
        ValueKind::Boolean => {
            if value.cardinality == Cardinality::Single {
                return Ok(direct_field_plan(id, specs[field_index].rust_ty.clone()));
            }
            let ty = array_param_ty(quote!(bool), value.cardinality)?;
            let storage = format_ident!("__ba{}_storage", field_index);
            let tmp = format_ident!("__ba{}", field_index);
            let temp = quote! {
                let #storage: ::std::vec::Vec<i32> =
                    #id.iter().copied().map(i32::from).collect();
                let #tmp: &[i32] = &#storage;
            };
            let doc_ref = id.to_string();
            Ok(FieldPlan {
                param: Some((id, ty)),
                temp: Some(temp),
                call: tmp,
                doc_ref,
            })
        }
        ValueKind::String { .. } => {
            plan_string_value(field_index, value, id, idents, specs, codegen)
        }
        ValueKind::Binary { length } => {
            if value.cardinality == Cardinality::Single {
                return Ok(match length {
                    ElementLength::Fixed(length) => direct_field_plan(id, quote!(&[u8; #length])),
                    ElementLength::Implicit | ElementLength::FieldRef(_) => {
                        direct_field_plan(id, specs[field_index].rust_ty.clone())
                    }
                });
            }

            match length {
                ElementLength::Fixed(length) => {
                    let ty = array_param_ty(quote!([u8; #length]), value.cardinality)?;
                    let tmp = format_ident!("__fba{}", field_index);
                    let temp = quote! {
                        let #tmp: &[u8] = #id.as_flattened();
                    };
                    let doc_ref = id.to_string();
                    Ok(FieldPlan {
                        param: Some((id, ty)),
                        temp: Some(temp),
                        call: tmp,
                        doc_ref,
                    })
                }
                ElementLength::FieldRef(_) => {
                    let ty = array_param_ty(quote!(&[u8]), value.cardinality)?;
                    let storage = format_ident!("__vba{}_storage", field_index);
                    let tmp = format_ident!("__vba{}", field_index);
                    let temp = quote! {
                        let #storage: ::std::vec::Vec<u8> = #id
                            .iter()
                            .flat_map(|value| value.iter().copied())
                            .collect();
                        let #tmp: &[u8] = &#storage;
                    };
                    let doc_ref = id.to_string();
                    Ok(FieldPlan {
                        param: Some((id, ty)),
                        temp: Some(temp),
                        call: tmp,
                        doc_ref,
                    })
                }
                ElementLength::Implicit => {
                    anyhow::bail!("internal error: binary array has no element length")
                }
            }
        }
        ValueKind::Sid => {
            if value.cardinality == Cardinality::Single {
                return Ok(direct_field_plan(id, specs[field_index].rust_ty.clone()));
            }
            let sid_ty = quote!(&#runtime::field::Sid);
            let ty = array_param_ty(sid_ty, value.cardinality)?;
            let storage = format_ident!("__sa{}_storage", field_index);
            let tmp = format_ident!("__sa{}", field_index);
            let temp = quote! {
                let mut #storage = ::std::vec::Vec::<u8>::new();
                for value in #id {
                    #storage.extend_from_slice(value.as_bytes());
                }
                let #tmp: &[u8] = &#storage;
            };
            let doc_ref = id.to_string();
            Ok(FieldPlan {
                param: Some((id, ty)),
                temp: Some(temp),
                call: tmp,
                doc_ref,
            })
        }
    }
}

/// Resolves field dependencies, then produces the public parameter and serialization plan for
/// each manifest field.
fn plan_event(
    ev: &Event,
    p: &Provider,
    specs: &[FieldSpec],
    codegen: &CodegenContext,
) -> anyhow::Result<Vec<FieldPlan>> {
    let n = ev.params.len();

    let mut index_by_name: HashMap<&str, usize> = HashMap::new();
    for (index, field) in ev.params.iter().enumerate() {
        index_by_name.entry(field.name.as_str()).or_insert(index);
    }

    let mut roles = (0..n)
        .map(|index| build_value_plan(&index_by_name, ev, p, index).map(FieldRole::Value))
        .collect::<anyhow::Result<Vec<_>>>()?;

    // A referenced binary length is derived from exactly one binary field.
    for index in 0..n {
        let length_field = match &roles[index] {
            FieldRole::Value(ValuePlan {
                kind:
                    ValueKind::Binary {
                        length: ElementLength::FieldRef(length_field),
                    },
                ..
            }) => Some(*length_field),
            _ => None,
        };
        let Some(length_field) = length_field else {
            continue;
        };

        match &roles[length_field] {
            FieldRole::Value(_) => {
                roles[length_field] = FieldRole::Derived(DerivedPlan::BinaryLength { from: index });
            }
            FieldRole::Derived(existing) => {
                let other = match existing {
                    DerivedPlan::BinaryLength { from } => &ev.params[*from].name,
                    DerivedPlan::ArrayCount { from } => &ev.params[from[0]].name,
                };
                anyhow::bail!(
                    "field {:?} in event {} of provider {} is referenced by multiple derived values ({} and {})",
                    ev.params[length_field].name,
                    ev.symbol,
                    p.symbol,
                    other,
                    ev.params[index].name
                );
            }
        }
    }

    // A count field may describe multiple arrays; generated code checks that their lengths agree.
    for (index, field) in ev.params.iter().enumerate() {
        let Count::FieldRef(name) = &field.count else {
            continue;
        };
        let count_field = *index_by_name.get(name.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "array field {} in event {} of provider {} references unknown count field {:?}",
                field.name,
                ev.symbol,
                p.symbol,
                name
            )
        })?;
        if count_field >= index {
            anyhow::bail!(
                "array field {} in event {} of provider {} references count field {:?}, which must appear before it",
                field.name,
                ev.symbol,
                p.symbol,
                name
            );
        }
        if !is_length_int(&ev.params[count_field].win_type)
            || ev.params[count_field].count != Count::Single
        {
            anyhow::bail!(
                "count field {:?} for array field {} in event {} of provider {} must be a scalar win:UInt8, win:UInt16, win:UInt32, or win:HexInt32",
                name,
                field.name,
                ev.symbol,
                p.symbol
            );
        }

        roles[count_field] = match roles[count_field].clone() {
            FieldRole::Value(_) => {
                FieldRole::Derived(DerivedPlan::ArrayCount { from: vec![index] })
            }
            FieldRole::Derived(DerivedPlan::ArrayCount { mut from }) => {
                from.push(index);
                FieldRole::Derived(DerivedPlan::ArrayCount { from })
            }
            FieldRole::Derived(DerivedPlan::BinaryLength { from }) => {
                anyhow::bail!(
                    "field {:?} in event {} of provider {} is used as both a binary length and an array count ({} and {})",
                    name,
                    ev.symbol,
                    p.symbol,
                    ev.params[from].name,
                    field.name
                )
            }
        };
    }

    // Assign identifiers up front so derived fields can name later caller-provided values.
    let mut idents: Vec<Ident> = Vec::with_capacity(n);
    let mut exposed_names: HashMap<String, String> = HashMap::new();
    for (index, (field, role)) in ev.params.iter().zip(&roles).enumerate() {
        let ident = match role {
            FieldRole::Derived(DerivedPlan::BinaryLength { .. }) => {
                format_ident!("__len{}", index)
            }
            FieldRole::Derived(DerivedPlan::ArrayCount { .. }) => {
                format_ident!("__count{}", index)
            }
            FieldRole::Value(_) => {
                let name = safe_ident(&ccase!(snake, &field.name));
                if let Some(previous) = exposed_names.insert(name.clone(), field.name.clone()) {
                    anyhow::bail!(
                        "param name {} collides with {} in event {} of provider {} (both transform to: {})",
                        field.name,
                        previous,
                        ev.symbol,
                        p.symbol,
                        name
                    );
                }
                format_ident!("{}", name)
            }
        };
        idents.push(ident);
    }

    let runtime = &codegen.runtime;
    let mut plans = Vec::with_capacity(n);
    for index in 0..n {
        let id = idents[index].clone();
        let plan = match &roles[index] {
            FieldRole::Value(value) => {
                plan_value_field(index, *value, id, &idents, ev, specs, codegen)?
            }
            FieldRole::Derived(DerivedPlan::BinaryLength { from }) => {
                let int_ty = specs[index].rust_ty.clone();
                let blob = idents[*from].clone();
                let source_is_array = Cardinality::from_count(&ev.params[*from].count).is_array();
                let temp = if source_is_array {
                    quote! {
                        let #id: #int_ty =
                            #runtime::field::checked_len(#runtime::field::uniform_len(#blob)?)?;
                    }
                } else {
                    quote! {
                        let #id: #int_ty = #runtime::field::checked_len(#blob.len())?;
                    }
                };
                let doc_ref = format!("{}.len", blob);
                FieldPlan {
                    param: None,
                    temp: Some(temp),
                    call: id,
                    doc_ref,
                }
            }
            FieldRole::Derived(DerivedPlan::ArrayCount { from }) => {
                let first = *from
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("internal error: array count has no source"))?;
                let int_ty = specs[index].rust_ty.clone();
                let array = idents[first].clone();
                let matching_lengths = from.iter().skip(1).map(|source| {
                    let other = &idents[*source];
                    quote! {
                        #runtime::field::ensure_len(#other.len(), #array.len())?;
                    }
                });
                let temp = quote! {
                    #(#matching_lengths)*
                    let #id: #int_ty = #runtime::field::checked_len(#array.len())?;
                };
                let doc_ref = format!("{}.len", array);
                FieldPlan {
                    param: None,
                    temp: Some(temp),
                    call: id,
                    doc_ref,
                }
            }
        };
        plans.push(plan);
    }

    Ok(plans)
}

/// Generates doc comments in the following form:
/// ```text
/// Writes the `EVENT_SYMBOL` event.
///
/// > The message text with {params} replaced.
/// > Inferred length parameters display as {bin_field.len}.
/// ```
fn event_doc(ev: &Event, field_refs: &[String]) -> TokenStream2 {
    let summary = format!("Writes the `{}` event.", ev.symbol);
    let message = ev.message.as_ref().map(|m| {
        // Render the message as a blockquote, render_message escapes Markdown control characters
        let line = format!("> {}", render_message(m, field_refs));
        quote! {
            #[doc = ""]
            #[doc = #line]
        }
    });
    quote! {
        #[doc = #summary]
        #message
    }
}

/// Rewrites an ETW message's `%N` placeholders into the form `{field_name}`.
fn render_message(msg: &str, field_refs: &[String]) -> String {
    let bytes = msg.as_bytes();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            // Copy one whole character so multibyte UTF-8 is not split. Every branch advances by
            // whole characters, so `i` is always on a character boundary and this always yields.
            let Some(ch) = msg[i..].chars().next() else {
                break;
            };
            push_escaped_markdown(&mut out, ch);
            i += ch.len_utf8();
            continue;
        }

        // "%%" represents an escaped percent sign
        if bytes.get(i + 1) == Some(&b'%') {
            push_escaped_markdown(&mut out, '%');
            i += 2;
            continue;
        }

        // "%<digits>" is a field placeholder
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j == i + 1 {
            // Leave a lone "%" or "%<letter>" as literal text
            push_escaped_markdown(&mut out, '%');
            i += 1;
            continue;
        }

        // The digits are ASCII by construction, but a run too long for usize overflows. Such a
        // placeholder can only be out of range, so it falls through to the None arm below.
        let n: Option<usize> = msg[i + 1..j].parse().ok();
        // Consume an optional "!..!" format specifier immediately following the number
        let mut end = j;
        if bytes.get(end) == Some(&b'!')
            && let Some(rel) = msg[end + 1..].find('!')
        {
            end = end + 1 + rel + 1;
        }
        match n
            .and_then(|n| n.checked_sub(1))
            .and_then(|idx| field_refs.get(idx))
        {
            Some(r) => {
                out.push('{');
                out.push_str(r);
                out.push('}');
            }
            // Preserve an out-of-range placeholder as escaped literal text
            None => msg[i..end]
                .chars()
                .for_each(|ch| push_escaped_markdown(&mut out, ch)),
        }
        i = end;
    }
    out
}

fn push_escaped_markdown(out: &mut String, ch: char) {
    if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '~') {
        out.push('\\');
    }
    out.push(ch);
}

fn gen_event_method(
    ev: &Event,
    specs: &[FieldSpec],
    helper: &str,
    p: &Provider,
    codegen: &CodegenContext,
) -> anyhow::Result<(String, TokenStream2)> {
    let method_ident_str = safe_ident(&ccase!(snake, &ev.symbol));
    let method = format_ident!("{}", method_ident_str);
    let helper = format_ident!("{}", helper);

    let plans = plan_event(ev, p, specs, codegen)?;

    let params = plans
        .iter()
        .filter_map(|fp| fp.param.as_ref().map(|(id, ty)| quote! { #id: #ty }));

    let temps = plans.iter().filter_map(|fp| fp.temp.as_ref());

    let call_args = plans.iter().map(|fp| &fp.call);

    let (id, version, channel, level, opcode) =
        (ev.id, ev.version, ev.channel, ev.level, ev.opcode);
    let (task, keyword) = (ev.task, ev.keyword);

    let field_refs: Vec<String> = plans.iter().map(|fp| fp.doc_ref.clone()).collect();
    let doc = event_doc(ev, &field_refs);
    let runtime = &codegen.runtime;

    Ok((
        method_ident_str,
        quote! {
            #doc
            #[allow(clippy::too_many_arguments)]
            pub fn #method(&self, #(#params),*) -> #runtime::Result<()> {
                const DESC: #runtime::EVENT_DESCRIPTOR =
                    #runtime::EVENT_DESCRIPTOR {
                        Id: #id,
                        Version: #version,
                        Channel: #channel,
                        Level: #level,
                        Opcode: #opcode,
                        Task: #task,
                        Keyword: #keyword,
                    };
                if !self.ctx.enabled(#level, #keyword) {
                    return #runtime::Result::Ok(());
                }
                #(#temps)*
                self.#helper(&DESC, #(#call_args),*)
            }
        },
    ))
}

/// Builds a shared helper function of the form
/// `fn __write_...(&self, desc, arg0: T0, ...) -> Result<()>`.
fn gen_backing(name: &str, specs: &[FieldSpec], codegen: &CodegenContext) -> TokenStream2 {
    let name = format_ident!("{}", name);
    let runtime = &codegen.runtime;

    let args: Vec<_> = (0..specs.len())
        .map(|i| format_ident!("arg{}", i))
        .collect();
    let params = args.iter().zip(specs.iter()).map(|(a, s)| {
        let ty = &s.rust_ty;
        quote! { #a: #ty }
    });

    // Store owned temporaries for each argument and the descriptor expression
    let mut temps = Vec::new();
    let mut descs = Vec::new();
    for (i, spec) in specs.iter().enumerate() {
        let arg = &args[i];
        match spec.class {
            FieldClass::Scalar => {
                descs.push(quote! { #runtime::field::scalar(&#arg) });
            }
            FieldClass::Bool => {
                let tmp = format_ident!("__b{}", i);
                temps.push(quote! { let #tmp: i32 = #arg as i32; });
                descs.push(quote! { #runtime::field::scalar(&#tmp) });
            }
            FieldClass::Str => {
                descs.push(quote! { #runtime::field::str16(#arg) });
            }
            FieldClass::AnsiStr => {
                descs.push(quote! { #runtime::field::str8(#arg) });
            }
            FieldClass::Bytes => {
                descs.push(quote! { #runtime::field::bytes(#arg) });
            }
            FieldClass::Slice => {
                descs.push(quote! { #runtime::field::slice(#arg) });
            }
            FieldClass::Sid => {
                descs.push(quote! { #runtime::field::sid(#arg) });
            }
        }
    }

    let n = specs.len();
    quote! {
        #[allow(clippy::too_many_arguments)]
        fn #name(
            &self,
            desc: &#runtime::EVENT_DESCRIPTOR,
            #(#params),*
        ) -> #runtime::Result<()> {
            #(#temps)*
            let data: [#runtime::field::EventDataDescriptor; #n] = [ #(#descs),* ];
            self.ctx.write(desc, &data)
        }
    }
}

/// How a field's Rust value becomes an `EVENT_DATA_DESCRIPTOR`.
#[derive(Clone, Copy)]
enum FieldClass {
    /// Passes a `Copy` type directly.
    Scalar,
    /// Encodes a `bool` as a Windows `BOOL`.
    Bool,
    /// Uses a NUL-terminated UTF-16LE buffer after the caller converts the `&str`.
    Str,
    /// Uses a NUL-terminated byte buffer after the caller converts the ANSI string.
    AnsiStr,
    /// Passes `&[u8]` through directly.
    Bytes,
    /// Passes a contiguous slice of fixed-size `Copy` values.
    Slice,
    /// Borrows a `field::Sid`.
    Sid,
}

struct FieldSpec {
    /// Provides a short fragment used to name the backing helper and deduplicate signatures.
    frag: &'static str,
    rust_ty: TokenStream2,
    class: FieldClass,
}

fn scalar_rust_ty(w: &WinType, codegen: &CodegenContext) -> Option<TokenStream2> {
    let runtime = &codegen.runtime;
    match w {
        WinType::Int8 => Some(quote!(i8)),
        WinType::UInt8 => Some(quote!(u8)),
        WinType::Int16 => Some(quote!(i16)),
        WinType::UInt16 => Some(quote!(u16)),
        WinType::Int32 => Some(quote!(i32)),
        WinType::UInt32 | WinType::HexInt32 => Some(quote!(u32)),
        WinType::Int64 => Some(quote!(i64)),
        WinType::UInt64 | WinType::HexInt64 => Some(quote!(u64)),
        WinType::FileTime => Some(quote!(#runtime::FILETIME)),
        WinType::Float => Some(quote!(f32)),
        WinType::Double => Some(quote!(f64)),
        WinType::Pointer => Some(quote!(usize)),
        WinType::Guid => Some(quote!(#runtime::GUID)),
        WinType::SystemTime => Some(quote!(#runtime::SYSTEMTIME)),
        _ => None,
    }
}

fn scalar_frag(w: &WinType, array: bool) -> Option<&'static str> {
    let (single, multiple) = match w {
        WinType::Int8 => ("c", "C"),
        WinType::UInt8 => ("u", "U"),
        WinType::Int16 => ("l", "L"),
        WinType::UInt16 => ("h", "H"),
        WinType::Int32 => ("d", "D"),
        WinType::UInt32 | WinType::HexInt32 => ("q", "Q"),
        WinType::Int64 => ("i", "I"),
        WinType::UInt64 | WinType::HexInt64 => ("x", "X"),
        WinType::FileTime => ("m", "M"),
        WinType::Float => ("f", "F"),
        WinType::Double => ("g", "G"),
        WinType::Pointer => ("p", "P"),
        WinType::Guid => ("j", "J"),
        WinType::SystemTime => ("y", "Y"),
        _ => return None,
    };
    Some(if array { multiple } else { single })
}

fn classify_single(w: &WinType, codegen: &CodegenContext) -> FieldSpec {
    let runtime = &codegen.runtime;
    if let Some(ty) = scalar_rust_ty(w, codegen) {
        return FieldSpec {
            frag: scalar_frag(w, false).expect("scalar type must have a fragment"),
            rust_ty: ty,
            class: FieldClass::Scalar,
        };
    }

    match w {
        WinType::Boolean => FieldSpec {
            frag: "t",
            rust_ty: quote!(bool),
            class: FieldClass::Bool,
        },
        WinType::UnicodeString(_) => FieldSpec {
            frag: "z",
            rust_ty: quote!(&[u16]),
            class: FieldClass::Str,
        },
        WinType::AnsiString(_, _) => FieldSpec {
            frag: "a",
            rust_ty: quote!(&[u8]),
            class: FieldClass::AnsiStr,
        },
        WinType::Binary(_) => FieldSpec {
            frag: "s",
            rust_ty: quote!(&[u8]),
            class: FieldClass::Bytes,
        },
        WinType::Sid => FieldSpec {
            frag: "k",
            rust_ty: quote!(&#runtime::field::Sid),
            class: FieldClass::Sid,
        },
        _ => unreachable!("all scalar types returned above"),
    }
}

fn classify(t: &crate::model::TypeInfo, codegen: &CodegenContext) -> FieldSpec {
    if t.count == Count::Single {
        return classify_single(&t.win_type, codegen);
    }

    if let Some(ty) = scalar_rust_ty(&t.win_type, codegen) {
        return FieldSpec {
            frag: scalar_frag(&t.win_type, true).expect("scalar type must have a fragment"),
            rust_ty: quote!(&[#ty]),
            class: FieldClass::Slice,
        };
    }

    match &t.win_type {
        WinType::Boolean => FieldSpec {
            frag: "T",
            rust_ty: quote!(&[i32]),
            class: FieldClass::Slice,
        },
        WinType::UnicodeString(_) => FieldSpec {
            frag: "Z",
            rust_ty: quote!(&[u16]),
            class: FieldClass::Slice,
        },
        WinType::AnsiString(_, _) | WinType::Binary(_) | WinType::Sid => FieldSpec {
            frag: "B",
            rust_ty: quote!(&[u8]),
            class: FieldClass::Bytes,
        },
        _ => unreachable!("all scalar types returned above"),
    }
}

fn backing_name(specs: &[FieldSpec]) -> String {
    if specs.is_empty() {
        return "__write".to_owned();
    }

    let mut s = String::from("__write_");
    for spec in specs {
        s.push_str(spec.frag);
    }
    s
}

/// Replaces invalid identifier characters with underscores and prepends an underscore when
/// required.
fn clean_ident(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();

    if let Some(c) = chars.next() {
        if is_xid_start(c) {
            out.push(c);
        } else {
            out.push('_');
            if is_xid_continue(c) {
                out.push(c);
            }
        }
    }

    for c in chars {
        out.push(if is_xid_continue(c) { c } else { '_' });
    }

    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Converts a string into one that is a valid identifier. The logic here may create collisions.
fn safe_ident(s: &str) -> String {
    let ident = clean_ident(s);

    match ident.as_str() {
        // Special cases that "r#" will not handle get an underscore suffix
        "crate" | "self" | "Self" | "super" | "_" => {
            format!("{ident}_")
        }
        // Anything else that fails to parse gets the "r#" prefix
        _ if syn::parse_str::<syn::Ident>(&ident).is_err() => {
            format!("r#{ident}")
        }
        _ => ident,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Length, Provider, TypeInfo, WinType};

    #[test]
    fn safe_ident_transforms() {
        // Ordinary identifiers pass through untouched
        assert_eq!(safe_ident("version"), "version");
        // Escape eligible keywords with the "r#" prefix
        assert_eq!(safe_ident("type"), "r#type");
        // Give ineligible keywords and the lone underscore an underscore suffix
        assert_eq!(safe_ident("crate"), "crate_");
        assert_eq!(safe_ident("self"), "self_");
        assert_eq!(safe_ident("_"), "__");
        // Replace invalid characters, a leading non-XID_Start character gains an underscore
        assert_eq!(safe_ident("worker-count"), "worker_count");
        assert_eq!(safe_ident("3d"), "_3d");
        assert_eq!(safe_ident(""), "__");
    }

    fn event(params: Vec<(&str, WinType)>) -> Event {
        Event {
            symbol: "E".into(),
            id: 1,
            version: 0,
            level: 0,
            opcode: 0,
            task: 0,
            keyword: 0,
            channel: 0,
            params: params
                .into_iter()
                .map(|(name, win_type)| TypeInfo {
                    name: name.into(),
                    win_type,
                    count: Count::Single,
                })
                .collect(),
            message: None,
        }
    }

    #[test]
    fn event_method_carries_message_doc() {
        let mut e = event(vec![("A", WinType::UInt32)]);
        e.message = Some("Hello %1 world".into());
        let codegen = CodegenContext::canonical();
        let specs: Vec<_> = e.params.iter().map(|t| classify(t, &codegen)).collect();
        let p = Provider {
            symbol: "P".into(),
            guid: 0,
            events: vec![],
        };
        let (_, ts) = gen_event_method(&e, &specs, "__write_q", &p, &codegen).unwrap();
        let rendered = ts.to_string();
        // The summary line and resolved message become "#[doc = ...]" attributes
        // with "%1" rewritten to the parameter name
        assert!(rendered.contains("Writes the"));
        assert!(rendered.contains("Hello {a} world"));
    }

    #[test]
    fn render_message_rewrites_placeholders() {
        let refs = vec!["version".to_string(), "worker_count".to_string()];
        assert_eq!(
            render_message("Started %1 with %2 workers", &refs),
            "Started {version} with {worker_count} workers"
        );
        // "%%" collapses to "%", an out-of-range placeholder remains literal text
        assert_eq!(render_message("100%% done %9", &refs), "100% done %9");
        // A digit run too long for usize is out of range like any other
        assert_eq!(
            render_message("%99999999999999999999999999 done", &refs),
            "%99999999999999999999999999 done"
        );
        // An "!..!" format specifier after the number is dropped
        assert_eq!(render_message("%1!s! done", &refs), "{version} done");
        // A hidden length field surfaces as blob.len
        let len_refs = vec!["blob.len".to_string(), "blob".to_string()];
        assert_eq!(
            render_message("%1 bytes in %2", &len_refs),
            "{blob.len} bytes in {blob}"
        );
    }

    #[test]
    fn render_message_escapes_markdown() {
        let refs = vec!["x".to_string()];
        // Inline formatting characters are escaped, incidental "(" and ")" characters are not
        // and the inserted "{...}" field reference is emitted verbatim
        assert_eq!(
            render_message("*b* _u_ [l](h) <t> %1", &refs),
            "\\*b\\* \\_u\\_ \\[l\\](h) \\<t> {x}"
        );
    }

    fn plan(e: &Event) -> anyhow::Result<Vec<FieldPlan>> {
        let codegen = CodegenContext::canonical();
        let p = Provider {
            symbol: "P".into(),
            guid: 0,
            events: vec![],
        };
        let specs: Vec<_> = e.params.iter().map(|t| classify(t, &codegen)).collect();
        plan_event(e, &p, &specs, &codegen)
    }

    fn exposed_count(plans: &[FieldPlan]) -> usize {
        plans.iter().filter(|fp| fp.param.is_some()).count()
    }

    #[test]
    fn fieldref_hides_and_derives_length() {
        let e = event(vec![
            ("BlobSize", WinType::UInt32),
            ("Blob", WinType::Binary(Length::FieldRef("BlobSize".into()))),
        ]);
        let plans = plan(&e).unwrap();
        // BlobSize is hidden and derived with a temporary before the call
        assert!(plans[0].param.is_none(), "BlobSize should be hidden");
        assert!(plans[0].temp.is_some(), "BlobSize should be derived");
        assert!(plans[0].call.to_string().starts_with("__len"));
        // Blob stays exposed
        assert!(plans[1].param.is_some());
        assert_eq!(exposed_count(&plans), 1);
    }

    #[test]
    fn constant_binary_becomes_fixed_array() {
        let e = event(vec![("Blob", WinType::Binary(Length::Constant(16)))]);
        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Blob should be exposed");
        assert!(ty.to_string().contains("16"));
        assert!(plans[0].temp.is_none());
    }

    #[test]
    fn constant_unicode_becomes_fixed_len() {
        let e = event(vec![("Name", WinType::UnicodeString(Length::Constant(4)))]);
        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Name should be exposed");
        assert!(ty.to_string().contains("str"));
        // A fixed-length temporary is emitted and passed in place of the raw parameter
        let temp = plans[0]
            .temp
            .as_ref()
            .expect("expected a fixed-length temp");
        assert!(temp.to_string().contains("to_u16cstring_fixed_len"));
        assert!(plans[0].call.to_string().starts_with("__f"));
    }

    #[test]
    fn utf8_ansi_string_is_converted() {
        let e = event(vec![(
            "Name",
            WinType::AnsiString(Length::Implicit, AnsiEncoding::Utf8),
        )]);
        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Name should be exposed");
        assert!(ty.to_string().contains("str"));
        let temp = plans[0].temp.as_ref().expect("expected a UTF-8 temp");
        assert!(temp.to_string().contains("to_cstring"));
        assert!(plans[0].call.to_string().starts_with("__a"));
    }

    #[test]
    fn provider_ansi_string_remains_encoded_bytes() {
        let e = event(vec![(
            "Name",
            WinType::AnsiString(Length::Implicit, AnsiEncoding::ProviderAnsi),
        )]);
        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Name should be exposed");
        assert!(ty.to_string().contains("u8"));
        assert!(plans[0].temp.is_none());
    }

    #[test]
    fn constant_provider_ansi_becomes_fixed_byte_array() {
        let e = event(vec![(
            "Name",
            WinType::AnsiString(Length::Constant(4), AnsiEncoding::ProviderAnsi),
        )]);
        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Name should be exposed");
        assert!(ty.to_string().contains("u8"));
        assert!(ty.to_string().contains('4'));
        assert!(plans[0].temp.is_none());
    }

    #[test]
    fn constant_utf8_ansi_becomes_fixed_len() {
        let e = event(vec![(
            "Name",
            WinType::AnsiString(Length::Constant(4), AnsiEncoding::Utf8),
        )]);
        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Name should be exposed");
        assert!(ty.to_string().contains("str"));
        let temp = plans[0]
            .temp
            .as_ref()
            .expect("expected a fixed-length ANSI temp");
        assert!(temp.to_string().contains("to_cstring_fixed_len"));
        assert!(plans[0].call.to_string().starts_with("__af"));
    }

    #[test]
    fn fieldref_unknown_target_errors() {
        let e = event(vec![(
            "Blob",
            WinType::Binary(Length::FieldRef("Nope".into())),
        )]);
        assert!(plan(&e).is_err());
    }

    #[test]
    fn fieldref_forward_reference_errors() {
        let e = event(vec![
            ("Blob", WinType::Binary(Length::FieldRef("BlobSize".into()))),
            ("BlobSize", WinType::UInt32),
        ]);
        assert!(plan(&e).is_err());
    }

    #[test]
    fn fieldref_non_integer_length_errors() {
        let e = event(vec![
            ("NotAnInt", WinType::Float),
            ("Blob", WinType::Binary(Length::FieldRef("NotAnInt".into()))),
        ]);
        assert!(plan(&e).is_err());
    }

    #[test]
    fn fieldref_64_bit_length_errors() {
        for ty in [WinType::UInt64, WinType::HexInt64] {
            let e = event(vec![
                ("TooWide", ty),
                ("Blob", WinType::Binary(Length::FieldRef("TooWide".into()))),
            ]);
            assert!(plan(&e).is_err());
        }
    }

    #[test]
    fn fieldref_shared_by_two_blobs_errors() {
        let e = event(vec![
            ("Size", WinType::UInt32),
            ("A", WinType::Binary(Length::FieldRef("Size".into()))),
            ("B", WinType::Binary(Length::FieldRef("Size".into()))),
        ]);
        assert!(plan(&e).is_err());
    }

    #[test]
    fn fixed_count_scalar_becomes_array_reference() {
        let mut e = event(vec![("Values", WinType::UInt32)]);
        e.params[0].count = Count::Constant(3);

        let plans = plan(&e).unwrap();
        let (_, ty) = plans[0].param.as_ref().expect("Values should be exposed");
        let ty = ty.to_string();
        assert!(ty.contains("u32"));
        assert!(ty.contains('3'));
        assert!(plans[0].temp.is_none());
    }

    #[test]
    fn value_plan_keeps_array_dimensions_orthogonal() {
        let mut e = event(vec![("Names", WinType::UnicodeString(Length::Constant(4)))]);
        e.params[0].count = Count::Constant(2);
        let p = Provider {
            symbol: "P".into(),
            guid: 0,
            events: vec![],
        };
        let index_by_name = HashMap::from([("Names", 0)]);

        let value = build_value_plan(&index_by_name, &e, &p, 0).unwrap();

        assert!(matches!(value.cardinality, Cardinality::Fixed(2)));
        assert!(matches!(
            value.kind,
            ValueKind::String {
                encoding: StringEncoding::Unicode,
                length: ElementLength::Fixed(4),
            }
        ));
    }

    #[test]
    fn count_field_is_hidden_and_derived() {
        let mut e = event(vec![
            ("ValueCount", WinType::UInt16),
            ("Values", WinType::UInt32),
        ]);
        e.params[1].count = Count::FieldRef("ValueCount".into());

        let plans = plan(&e).unwrap();
        assert!(plans[0].param.is_none(), "ValueCount should be hidden");
        assert!(plans[0].temp.is_some(), "ValueCount should be derived");
        let (_, ty) = plans[1].param.as_ref().expect("Values should be exposed");
        assert!(ty.to_string().contains("[u32]"));
    }

    #[test]
    fn shared_count_field_checks_array_lengths() {
        let mut e = event(vec![
            ("ValueCount", WinType::UInt16),
            ("Primary", WinType::UInt32),
            ("Secondary", WinType::UInt16),
        ]);
        e.params[1].count = Count::FieldRef("ValueCount".into());
        e.params[2].count = Count::FieldRef("ValueCount".into());

        let plans = plan(&e).unwrap();
        let temp = plans[0]
            .temp
            .as_ref()
            .expect("ValueCount should be derived")
            .to_string();
        assert!(temp.contains("ensure_len"));
        assert_eq!(exposed_count(&plans), 2);
    }

    #[test]
    fn count_field_reference_is_validated() {
        let mut unknown = event(vec![("Values", WinType::UInt32)]);
        unknown.params[0].count = Count::FieldRef("Missing".into());
        assert!(plan(&unknown).is_err());

        let mut forward = event(vec![
            ("Values", WinType::UInt32),
            ("ValueCount", WinType::UInt16),
        ]);
        forward.params[0].count = Count::FieldRef("ValueCount".into());
        assert!(plan(&forward).is_err());

        let mut non_integer = event(vec![
            ("ValueCount", WinType::Float),
            ("Values", WinType::UInt32),
        ]);
        non_integer.params[1].count = Count::FieldRef("ValueCount".into());
        assert!(plan(&non_integer).is_err());
    }
}
