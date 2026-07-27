//! Procedural macro implementation for `etw-wrapper`.
//!
//! See [`gen_etw_wrapper!`] for the supported syntax and generated API.

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
use syn::{Ident, LitBool, LitStr, Meta, Token, parenthesized};
use unicode_ident::{is_xid_continue, is_xid_start};

/// The parsed macro input, including the manifest path, event error behavior, and optional
/// wrapper name overrides.
struct WrapperArgs {
    path: LitStr,
    event_errors: EventErrors,
    input_panics: PanicConfig,
    write_panics: PanicConfig,
    overrides: Vec<NameOverride>,
}

/// How generated event methods expose errors to their callers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EventErrors {
    /// Return the error to the caller as a `Result`.
    #[default]
    Propagate,
    /// Deliberately discard errors from event preparation and writing.
    Ignore,
}

#[derive(Default)]
struct PanicConfig {
    enabled: bool,
    when: Option<Meta>,
}

#[derive(Clone, Copy)]
struct PanicPolicy<'a> {
    enabled: bool,
    when: Option<&'a Meta>,
}

#[derive(Clone, Copy)]
struct EventPolicy<'a> {
    errors: EventErrors,
    input_panics: PanicPolicy<'a>,
    write_panics: PanicPolicy<'a>,
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

fn parse_cfg_predicate(input: ParseStream, option: &str) -> syn::Result<Meta> {
    let cfg: Ident = input.parse()?;
    if cfg != "cfg" {
        return Err(syn::Error::new(
            cfg.span(),
            format!("`{option}` must be a `cfg(...)` predicate"),
        ));
    }
    let content;
    parenthesized!(content in input);
    content.parse()
}

fn parse_panic_config(input: ParseStream, option: &str) -> syn::Result<PanicConfig> {
    if input.peek(LitBool) {
        let enabled = input.parse::<LitBool>()?.value;
        return Ok(PanicConfig {
            enabled,
            when: None,
        });
    }

    let when = parse_cfg_predicate(input, option).map_err(|_| {
        syn::Error::new(
            input.span(),
            format!("`{option}` must be `true`, `false`, or a `cfg(...)` predicate"),
        )
    })?;
    Ok(PanicConfig {
        enabled: true,
        when: Some(when),
    })
}

impl Parse for WrapperArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let path: LitStr = input.parse()?;
        let mut event_errors = EventErrors::default();
        let mut event_errors_set = false;
        let mut input_panics = PanicConfig::default();
        let mut input_panics_set = false;
        let mut write_panics = PanicConfig::default();
        let mut write_panics_set = false;
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
                let key = ident.to_string();
                let key_span = ident.span();

                if input.peek(Token![=]) {
                    input.parse::<Token![=]>()?;
                    match key.as_str() {
                        "event_methods_return_unit" => {
                            if event_errors_set {
                                return Err(syn::Error::new(
                                    key_span,
                                    "duplicate `event_methods_return_unit` option",
                                ));
                            }

                            event_errors = if input.parse::<LitBool>()?.value {
                                EventErrors::Ignore
                            } else {
                                EventErrors::Propagate
                            };
                            event_errors_set = true;
                        }
                        "panic_on_input" => {
                            if input_panics_set {
                                return Err(syn::Error::new(
                                    key_span,
                                    "duplicate `panic_on_input` option",
                                ));
                            }

                            input_panics = parse_panic_config(input, "panic_on_input")?;
                            input_panics_set = true;
                        }
                        "panic_on_write" => {
                            if write_panics_set {
                                return Err(syn::Error::new(
                                    key_span,
                                    "duplicate `panic_on_write` option",
                                ));
                            }

                            write_panics = parse_panic_config(input, "panic_on_write")?;
                            write_panics_set = true;
                        }
                        _ => {
                            return Err(syn::Error::new(
                                key_span,
                                format!("unknown gen_etw_wrapper option `{key}`"),
                            ));
                        }
                    }
                    continue;
                }

                (key, key_span)
            };
            input.parse::<Token![->]>()?;
            let name: Ident = input.parse()?;
            overrides.push(NameOverride {
                key,
                key_span,
                name,
            });
        }

        Ok(WrapperArgs {
            path,
            event_errors,
            input_panics,
            write_panics,
            overrides,
        })
    }
}

/// Generates strongly typed ETW providers from an instrumentation manifest.
///
/// Invoke this macro in item position with a path to an ETW manifest:
///
/// ```ignore
/// use etw_wrapper::gen_etw_wrapper;
///
/// gen_etw_wrapper!("manifests/widgetservice.man");
/// ```
///
/// The path is resolved relative to the invoking crate's `CARGO_MANIFEST_DIR`. Changes to the
/// manifest are tracked by Cargo and cause the crate to be rebuilt.
///
/// # Generated API
///
/// The macro generates one public provider struct for each provider in the manifest. By default,
/// its name is the provider's `symbol` converted to PascalCase with `Logger` appended. For example,
/// `PROVIDER_WIDGETSERVICE` produces `ProviderWidgetserviceLogger`.
///
/// Each provider struct has:
///
/// - a `register()` associated function that registers the provider with ETW;
/// - one snake_case method per manifest event, with parameters derived from the event template;
/// - automatic provider unregistration when the value is dropped.
///
/// Event methods return [`etw_wrapper::Result<()>`](https://docs.rs/etw-wrapper/latest/etw_wrapper/type.Result.html).
/// Manifest fields used as a `count` reference, or as the `length` of a `win:Binary` field, are
/// derived from the corresponding slice and omitted from the Rust method signature. The `length`
/// reference of a string field sets the width of the encoded string, so it stays a parameter.
///
/// # Event errors
///
/// By default, event methods return `etw_wrapper::Result<()>`. Applications that treat logging as
/// best-effort can generate event methods that return `()` and discard errors:
///
/// ```ignore
/// # use etw_wrapper::gen_etw_wrapper;
/// gen_etw_wrapper!(
///     "manifests/widgetservice.man",
///     event_methods_return_unit = true,
///     panic_on_input = cfg(debug_assertions),
/// );
/// ```
///
/// `panic_on_input` covers errors detected while validating or preparing caller-provided values.
/// `panic_on_write` covers errors returned by the Windows event-write call. Each accepts `true`,
/// `false` (the default), or a Rust `cfg(...)` predicate. When a panic behavior is disabled, event
/// errors are returned unless `event_methods_return_unit = true`. Only
/// `event_methods_return_unit` affects the generated method's return type.
///
/// This setting affects only event methods. Provider registration remains fallible.
///
/// See the repository's
/// [type mapping](https://github.com/austinwagner/etw-wrapper-rs#type-mapping) for the complete
/// mapping from manifest `inType`, `length`, and `count` attributes to Rust parameter types.
///
/// # Naming providers
///
/// Map a provider symbol to a Rust identifier to override the generated struct name:
///
/// ```ignore
/// # use etw_wrapper::gen_etw_wrapper;
/// gen_etw_wrapper!(
///     "manifests/widgetservice.man",
///     PROVIDER_WIDGETSERVICE -> WidgetLogger,
/// );
/// ```
///
/// Quote a provider symbol when it is not a valid Rust identifier. A manifest with multiple
/// providers may contain multiple comma-separated overrides:
///
/// ```ignore
/// # use etw_wrapper::gen_etw_wrapper;
/// gen_etw_wrapper!(
///     "manifests/providers.man",
///     "Contoso.WidgetService" -> WidgetLogger,
///     PROVIDER_DATABASE -> DatabaseLogger,
/// );
/// ```
///
/// An override that does not match a provider symbol is a compile error.
///
/// # Example
///
/// Given a manifest that declares `PROVIDER_WIDGETSERVICE` with a `SERVICE_STARTED` event, an
/// invocation can be used as follows:
///
/// ```ignore
/// use etw_wrapper::{FILETIME, gen_etw_wrapper};
///
/// gen_etw_wrapper!(
///     "manifests/widgetservice.man",
///     PROVIDER_WIDGETSERVICE -> WidgetLogger,
/// );
///
/// fn emit() -> etw_wrapper::Result<()> {
///     let logger = WidgetLogger::register()?;
///     logger.service_started("1.0.0", 8, FILETIME::default())
/// }
/// ```
///
/// # Provider resources
///
/// This macro emits events but does not compile or register the provider's message and metadata
/// resource tables. Applications that need decoded event names, fields, and messages must compile
/// and register those resources separately, for example with `mc.exe` and `wevtutil.exe`.
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
    let event_policy = EventPolicy {
        errors: args.event_errors,
        input_panics: PanicPolicy {
            enabled: args.input_panics.enabled,
            when: args.input_panics.when.as_ref(),
        },
        write_panics: PanicPolicy {
            enabled: args.write_panics.enabled,
            when: args.write_panics.when.as_ref(),
        },
    };

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
        .map(|p| {
            gen_provider(
                p,
                overrides.get(p.symbol.as_str()).copied(),
                event_policy,
                &codegen,
            )
        })
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
    event_policy: EventPolicy<'_>,
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
        let (method_ident, method) =
            gen_event_method(ev, &specs, &helper, p, event_policy, codegen)?;
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
    /// Fallible checks that can run before the provider enablement fast path.
    validation: Option<TokenStream2>,
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

/// Resolves a `length="..."` or `count="..."` reference (`role`) from the field at `field_index`
/// (described as `subject` in errors) to the index of a scalar integer field declared before it.
fn resolve_prior_int_field(
    index_by_name: &HashMap<&str, usize>,
    ev: &Event,
    p: &Provider,
    field_index: usize,
    name: &str,
    subject: &str,
    role: &str,
) -> anyhow::Result<usize> {
    let field = &ev.params[field_index];
    let index = *index_by_name.get(name).ok_or_else(|| {
        anyhow::anyhow!(
            "{subject} {} in event {} of provider {} references unknown {role} field {:?}",
            field.name,
            ev.symbol,
            p.symbol,
            name
        )
    })?;
    if index >= field_index {
        anyhow::bail!(
            "{subject} {} in event {} of provider {} references {role} field {:?}, which must appear before it",
            field.name,
            ev.symbol,
            p.symbol,
            name
        );
    }
    if !is_length_int(&ev.params[index].win_type) || ev.params[index].count != Count::Single {
        anyhow::bail!(
            "{role} field {:?} for {subject} {} in event {} of provider {} must be a scalar win:UInt8, win:UInt16, win:UInt32, or win:HexInt32",
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
            // A referenced length declares a fixed field width, so every encoded string must have
            // exactly that length. Decoders read the declared count and never scan for a NUL.
            Length::FieldRef(name) => Ok(ElementLength::FieldRef(resolve_prior_int_field(
                index_by_name,
                ev,
                p,
                field_index,
                name,
                "string field",
                "length",
            )?)),
        }
    };

    let kind = match &field.win_type {
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
                Length::FieldRef(name) => ElementLength::FieldRef(resolve_prior_int_field(
                    index_by_name,
                    ev,
                    p,
                    field_index,
                    name,
                    "binary field",
                    "length",
                )?),
            },
        },
        WinType::Sid => ValueKind::Sid,
        // Every remaining WinType is a fixed-size scalar. A future non-scalar type missed here
        // would still panic during classification via scalar_info.
        _ => ValueKind::Scalar,
    };

    Ok(ValuePlan { kind, cardinality })
}

fn direct_field_plan(id: Ident, ty: TokenStream2) -> FieldPlan {
    let doc_ref = id.to_string();
    FieldPlan {
        param: Some((id.clone(), ty)),
        validation: None,
        temp: None,
        call: id,
        doc_ref,
    }
}

/// A field plan whose parameter is re-encoded into the temporary `tmp` by `temp` before the
/// backing helper call.
fn buffered_plan(id: Ident, ty: TokenStream2, tmp: Ident, temp: TokenStream2) -> FieldPlan {
    let doc_ref = id.to_string();
    FieldPlan {
        param: Some((id, ty)),
        validation: None,
        temp: Some(temp),
        call: tmp,
        doc_ref,
    }
}

fn with_validation(mut plan: FieldPlan, validation: TokenStream2) -> FieldPlan {
    plan.validation = Some(validation);
    plan
}

/// Plans an array parameter whose elements are flattened into one contiguous buffer of `elem_ty`.
/// The statements produced by `per_element` run once per element with `value` bound to the
/// element reference and the storage `Vec` identifier passed in.
fn accumulated_plan(
    field_index: usize,
    id: Ident,
    ty: TokenStream2,
    elem_ty: TokenStream2,
    per_element: impl FnOnce(&Ident) -> TokenStream2,
) -> FieldPlan {
    let storage = format_ident!("__t{}_storage", field_index);
    let tmp = format_ident!("__t{}", field_index);
    let body = per_element(&storage);
    let temp = quote! {
        let mut #storage = ::std::vec::Vec::<#elem_ty>::new();
        for value in #id {
            #body
        }
        let #tmp: &[#elem_ty] = &#storage;
    };
    buffered_plan(id, ty, tmp, temp)
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

    // A provider-ANSI string is already encoded, so the caller's bytes pass through directly.
    // The other encodings convert from `&str` with the paired (plain, fixed-length) runtime
    // functions into a buffer with the given element type.
    let encode_fns = match encoding {
        StringEncoding::ProviderAnsi => None,
        StringEncoding::Unicode => Some(("to_u16cstring", "to_u16cstring_fixed_len", quote!(u16))),
        StringEncoding::Utf8 => Some(("to_cstring", "to_cstring_fixed_len", quote!(u8))),
    };

    if value.cardinality == Cardinality::Single {
        let Some((plain, fixed, elem_ty)) = encode_fns else {
            let ty = match length {
                ElementLength::Fixed(length) => quote!(&[u8; #length]),
                ElementLength::Implicit | ElementLength::FieldRef(_) => {
                    specs[field_index].rust_ty.clone()
                }
            };
            let plan = direct_field_plan(id, ty);
            let input = plan.call.clone();
            // These bytes are already encoded, so a referenced length cannot be applied for the
            // caller; the buffer they hand over has to match the width the manifest declares.
            let validate_len = match length {
                ElementLength::FieldRef(from) => {
                    let declared = &idents[from];
                    quote! {
                        #runtime::field::ensure_len(#input.len(), #declared as usize)?;
                    }
                }
                ElementLength::Implicit | ElementLength::Fixed(_) => TokenStream2::new(),
            };
            return Ok(with_validation(
                plan,
                quote! {
                    #validate_len
                    #runtime::field::ensure_nul_terminated(#input)?;
                },
            ));
        };
        let plain = format_ident!("{plain}");
        let fixed = format_ident!("{fixed}");
        let (validation, encode) = match length {
            ElementLength::Fixed(length) => (
                Some(quote! {
                    #runtime::field::ensure_nonzero_length(#length)?;
                }),
                quote!(#runtime::field::#fixed(#id, #length)),
            ),
            ElementLength::FieldRef(from) => {
                let declared = &idents[from];
                (
                    Some(quote! {
                        #runtime::field::ensure_nonzero_length(#declared as usize)?;
                    }),
                    quote!(#runtime::field::#fixed(#id, #declared as usize)),
                )
            }
            ElementLength::Implicit => (None, quote!(#runtime::field::#plain(#id))),
        };
        let tmp = format_ident!("__t{}", field_index);
        let temp = quote! {
            let #tmp: &[#elem_ty] = &#encode;
        };
        let plan = buffered_plan(id, quote!(&str), tmp, temp);
        return Ok(match validation {
            Some(validation) => with_validation(plan, validation),
            None => plan,
        });
    }

    match encode_fns {
        Some((plain, fixed, elem_ty)) => {
            let plain = format_ident!("{plain}");
            let fixed = format_ident!("{fixed}");
            let ty = array_param_ty(quote!(&str), value.cardinality)?;
            let (validation, encode) = match length {
                ElementLength::Implicit => (None, quote!(#runtime::field::#plain(value))),
                ElementLength::Fixed(length) => (
                    Some(quote! {
                        #runtime::field::ensure_nonzero_length(#length)?;
                    }),
                    quote!(#runtime::field::#fixed(value, #length)),
                ),
                ElementLength::FieldRef(from) => {
                    let length = &idents[from];
                    (
                        Some(quote! {
                            #runtime::field::ensure_nonzero_length(#length as usize)?;
                        }),
                        quote!(#runtime::field::#fixed(value, #length as usize)),
                    )
                }
            };
            let plan = accumulated_plan(field_index, id, ty, elem_ty, |storage| {
                quote! { #storage.extend_from_slice(&#encode); }
            });
            Ok(match validation {
                Some(validation) => with_validation(plan, validation),
                None => plan,
            })
        }
        None => {
            let element_ty = match length {
                ElementLength::Fixed(length) => quote!([u8; #length]),
                ElementLength::Implicit | ElementLength::FieldRef(_) => quote!(&[u8]),
            };
            let ty = array_param_ty(element_ty, value.cardinality)?;
            let validate_len = match length {
                ElementLength::FieldRef(from) => {
                    let length = &idents[from];
                    quote! {
                        #runtime::field::ensure_len(value.len(), #length as usize)?;
                    }
                }
                ElementLength::Implicit | ElementLength::Fixed(_) => TokenStream2::new(),
            };
            let input = id.clone();
            let validation = quote! {
                for value in #input {
                    #validate_len
                    #runtime::field::ensure_nul_terminated(value)?;
                }
            };
            let plan = accumulated_plan(field_index, id, ty, quote!(u8), |storage| {
                quote! {
                    #storage.extend_from_slice(value);
                }
            });
            Ok(with_validation(plan, validation))
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
                let (_, _, element_ty) = scalar_info(&ev.params[field_index].win_type, codegen)
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
            Ok(accumulated_plan(
                field_index,
                id,
                ty,
                quote!(i32),
                |storage| {
                    quote! { #storage.push(i32::from(*value)); }
                },
            ))
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
                    let tmp = format_ident!("__t{}", field_index);
                    let temp = quote! {
                        let #tmp: &[u8] = #id.as_flattened();
                    };
                    Ok(buffered_plan(id, ty, tmp, temp))
                }
                ElementLength::FieldRef(_) => {
                    let ty = array_param_ty(quote!(&[u8]), value.cardinality)?;
                    Ok(accumulated_plan(
                        field_index,
                        id,
                        ty,
                        quote!(u8),
                        |storage| {
                            quote! { #storage.extend_from_slice(value); }
                        },
                    ))
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
            let ty = array_param_ty(quote!(&#runtime::field::Sid), value.cardinality)?;
            Ok(accumulated_plan(
                field_index,
                id,
                ty,
                quote!(u8),
                |storage| {
                    quote! { #storage.extend_from_slice(value.as_bytes()); }
                },
            ))
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
        let count_field =
            resolve_prior_int_field(&index_by_name, ev, p, index, name, "array field", "count")?;

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
                let (validation, temp) = if source_is_array {
                    (
                        quote! {
                            let _: #int_ty =
                                #runtime::field::checked_len(
                                    #runtime::field::uniform_len(#blob)?
                                )?;
                        },
                        quote! {
                            let #id: #int_ty =
                                #runtime::field::checked_len(
                                    #runtime::field::uniform_len(#blob)?
                                )?;
                        },
                    )
                } else {
                    (
                        quote! {
                            let _: #int_ty = #runtime::field::checked_len(#blob.len())?;
                        },
                        quote! {
                            let #id: #int_ty = #runtime::field::checked_len(#blob.len())?;
                        },
                    )
                };
                let doc_ref = format!("{}.len", blob);
                FieldPlan {
                    param: None,
                    validation: Some(validation),
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
                let matching_lengths: Vec<_> = from
                    .iter()
                    .skip(1)
                    .map(|source| {
                        let other = &idents[*source];
                        quote! {
                            #runtime::field::ensure_len(#other.len(), #array.len())?;
                        }
                    })
                    .collect();
                let validation = quote! {
                    #(#matching_lengths)*
                    let _: #int_ty = #runtime::field::checked_len(#array.len())?;
                };
                let temp = quote! {
                    #(#matching_lengths)*
                    let #id: #int_ty = #runtime::field::checked_len(#array.len())?;
                };
                let doc_ref = format!("{}.len", array);
                FieldPlan {
                    param: None,
                    validation: Some(validation),
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

fn event_error_fallback(event_errors: EventErrors, runtime: &TokenStream2) -> TokenStream2 {
    match event_errors {
        EventErrors::Propagate => quote! {
            #runtime::Result::Err(__error)
        },
        EventErrors::Ignore => quote! {
            ::core::mem::drop(__error);
        },
    }
}

fn event_error_handler(
    should_panic: bool,
    panic_when: Option<&Meta>,
    panic_message: &str,
    fallback: &TokenStream2,
) -> TokenStream2 {
    if !should_panic {
        return fallback.clone();
    }

    let panic = quote! {
        ::core::panic!(#panic_message, __error)
    };
    match panic_when {
        Some(predicate) => quote! {
            {
                #[cfg(#predicate)]
                {
                    #panic
                }
                #[cfg(not(#predicate))]
                {
                    #fallback
                }
            }
        },
        None => panic,
    }
}

fn gen_event_method(
    ev: &Event,
    specs: &[FieldSpec],
    helper: &str,
    p: &Provider,
    event_policy: EventPolicy<'_>,
    codegen: &CodegenContext,
) -> anyhow::Result<(String, TokenStream2)> {
    let method_ident_str = safe_ident(&ccase!(snake, &ev.symbol));
    let method = format_ident!("{}", method_ident_str);
    let helper = format_ident!("{}", helper);

    let plans = plan_event(ev, p, specs, codegen)?;

    let params: Vec<_> = plans
        .iter()
        .filter_map(|fp| fp.param.as_ref().map(|(id, ty)| quote! { #id: #ty }))
        .collect();

    let validations: Vec<_> = plans
        .iter()
        .filter_map(|fp| fp.validation.as_ref())
        .collect();
    let temps: Vec<_> = plans.iter().filter_map(|fp| fp.temp.as_ref()).collect();
    let call_args: Vec<_> = plans.iter().map(|fp| &fp.call).collect();

    let (id, version, channel, level, opcode) =
        (ev.id, ev.version, ev.channel, ev.level, ev.opcode);
    let (task, keyword) = (ev.task, ev.keyword);

    let field_refs: Vec<String> = plans.iter().map(|fp| fp.doc_ref.clone()).collect();
    let doc = event_doc(ev, &field_refs);
    let runtime = &codegen.runtime;
    let event_symbol = &ev.symbol;

    let prevalidation = if !validations.is_empty() && event_policy.input_panics.enabled {
        let validate = quote! {
            if let #runtime::Result::Err(__error) = (|| -> #runtime::Result<()> {
                #(#validations)*
                #runtime::Result::Ok(())
            })() {
                ::core::panic!(
                    "invalid input for ETW event `{}`: {}",
                    #event_symbol,
                    __error
                );
            }
        };
        match event_policy.input_panics.when {
            Some(predicate) => quote! {
                #[cfg(#predicate)]
                #validate
            },
            None => validate,
        }
    } else {
        TokenStream2::new()
    };

    let outcome = quote! {
        enum __EtwEventError {
            Input(#runtime::Error),
            Write(#runtime::Error),
        }

        impl ::core::convert::From<#runtime::Error> for __EtwEventError {
            fn from(error: #runtime::Error) -> Self {
                Self::Input(error)
            }
        }

        let __outcome: ::core::result::Result<(), __EtwEventError> = (|| {
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
                return ::core::result::Result::Ok(());
            }
            #(#validations)*
            #(#temps)*
            self.#helper(&DESC, #(#call_args),*)
                .map_err(__EtwEventError::Write)
        })();
    };

    let fallback = event_error_fallback(event_policy.errors, runtime);
    let input_message = format!("invalid input for ETW event `{event_symbol}`: {{}}");
    let write_message = format!("failed to write ETW event `{event_symbol}`: {{}}");
    let input_handler = event_error_handler(
        event_policy.input_panics.enabled,
        event_policy.input_panics.when,
        &input_message,
        &fallback,
    );
    let write_handler = event_error_handler(
        event_policy.write_panics.enabled,
        event_policy.write_panics.when,
        &write_message,
        &fallback,
    );
    let success = match event_policy.errors {
        EventErrors::Propagate => quote! { #runtime::Result::Ok(()) },
        EventErrors::Ignore => quote! {},
    };
    let handle_outcome = quote! {
        match __outcome {
            ::core::result::Result::Ok(()) => {
                #success
            }
            ::core::result::Result::Err(__EtwEventError::Input(__error)) => {
                #input_handler
            }
            ::core::result::Result::Err(__EtwEventError::Write(__error)) => {
                #write_handler
            }
        }
    };

    let method = match event_policy.errors {
        EventErrors::Propagate => quote! {
            #doc
            #[allow(clippy::too_many_arguments)]
            pub fn #method(&self, #(#params),*) -> #runtime::Result<()> {
                #prevalidation
                #outcome
                #handle_outcome
            }
        },
        EventErrors::Ignore => quote! {
            #doc
            ///
            /// Errors not configured to panic are ignored.
            #[allow(clippy::too_many_arguments)]
            pub fn #method(&self, #(#params),*) {
                #prevalidation
                #outcome
                #handle_outcome
            }
        },
    };

    Ok((method_ident_str, method))
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

/// The backing-helper name fragments (scalar and slice forms) and Rust type for a scalar
/// `WinType`. Returns `None` for variable-size types.
fn scalar_info(
    w: &WinType,
    codegen: &CodegenContext,
) -> Option<(&'static str, &'static str, TokenStream2)> {
    let runtime = &codegen.runtime;
    Some(match w {
        WinType::Int8 => ("c", "C", quote!(i8)),
        WinType::UInt8 => ("u", "U", quote!(u8)),
        WinType::Int16 => ("l", "L", quote!(i16)),
        WinType::UInt16 => ("h", "H", quote!(u16)),
        WinType::Int32 => ("d", "D", quote!(i32)),
        WinType::UInt32 | WinType::HexInt32 => ("q", "Q", quote!(u32)),
        WinType::Int64 => ("i", "I", quote!(i64)),
        WinType::UInt64 | WinType::HexInt64 => ("x", "X", quote!(u64)),
        WinType::FileTime => ("m", "M", quote!(#runtime::FILETIME)),
        WinType::Float => ("f", "F", quote!(f32)),
        WinType::Double => ("g", "G", quote!(f64)),
        WinType::Pointer => ("p", "P", quote!(usize)),
        WinType::Guid => ("j", "J", quote!(#runtime::GUID)),
        WinType::SystemTime => ("y", "Y", quote!(#runtime::SYSTEMTIME)),
        _ => return None,
    })
}

fn classify_single(w: &WinType, codegen: &CodegenContext) -> FieldSpec {
    let runtime = &codegen.runtime;
    if let Some((frag, _, rust_ty)) = scalar_info(w, codegen) {
        return FieldSpec {
            frag,
            rust_ty,
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

    if let Some((_, frag, ty)) = scalar_info(&t.win_type, codegen) {
        return FieldSpec {
            frag,
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
    fn wrapper_args_default_to_propagating_event_errors() {
        let args: WrapperArgs = syn::parse_str(r#""manifest.man", PROVIDER -> Logger"#).unwrap();

        assert_eq!(args.event_errors, EventErrors::Propagate);
        assert!(!args.input_panics.enabled);
        assert!(!args.write_panics.enabled);
        assert_eq!(args.overrides.len(), 1);
    }

    #[test]
    fn wrapper_args_parse_unit_return_setting() {
        let args: WrapperArgs = syn::parse_str(
            r#""manifest.man", event_methods_return_unit = true, PROVIDER -> Logger"#,
        )
        .unwrap();

        assert_eq!(args.event_errors, EventErrors::Ignore);
        assert_eq!(args.overrides.len(), 1);

        let args: WrapperArgs =
            syn::parse_str(r#""manifest.man", event_methods_return_unit = false"#).unwrap();
        assert_eq!(args.event_errors, EventErrors::Propagate);
    }

    #[test]
    fn wrapper_args_reject_invalid_or_duplicate_unit_return_setting() {
        assert!(
            syn::parse_str::<WrapperArgs>(
                r#""manifest.man", event_methods_return_unit = cfg(debug_assertions)"#
            )
            .is_err()
        );

        let duplicate = syn::parse_str::<WrapperArgs>(
            r#""manifest.man",
                event_methods_return_unit = true,
                event_methods_return_unit = false"#,
        )
        .err()
        .expect("duplicate option should fail");
        assert!(
            duplicate
                .to_string()
                .contains("duplicate `event_methods_return_unit` option")
        );
    }

    #[test]
    fn wrapper_args_parse_independent_panic_settings() {
        let args: WrapperArgs = syn::parse_str(
            r#""manifest.man",
                panic_on_input = cfg(debug_assertions),
                panic_on_write = true"#,
        )
        .unwrap();

        assert!(args.input_panics.enabled);
        assert!(args.write_panics.enabled);
        let input = args
            .input_panics
            .when
            .expect("input cfg predicate should be retained");
        assert!(args.write_panics.when.is_none());
        assert_eq!(
            quote!(#input).to_string(),
            quote!(debug_assertions).to_string()
        );
    }

    #[test]
    fn wrapper_args_parse_false_and_reject_invalid_panic_settings() {
        let args: WrapperArgs =
            syn::parse_str(r#""manifest.man", panic_on_input = false"#).unwrap();
        assert!(!args.input_panics.enabled);
        assert!(args.input_panics.when.is_none());

        let error = syn::parse_str::<WrapperArgs>(r#""manifest.man", panic_on_write = sometimes"#)
            .err()
            .expect("invalid panic setting should fail");

        assert!(
            error
                .to_string()
                .contains("must be `true`, `false`, or a `cfg(...)` predicate")
        );
    }

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
        let (_, ts) = gen_event_method(
            &e,
            &specs,
            "__write_q",
            &p,
            EventPolicy {
                errors: EventErrors::Propagate,
                input_panics: PanicPolicy {
                    enabled: false,
                    when: None,
                },
                write_panics: PanicPolicy {
                    enabled: false,
                    when: None,
                },
            },
            &codegen,
        )
        .unwrap();
        let rendered = ts.to_string();
        // The summary line and resolved message become "#[doc = ...]" attributes
        // with "%1" rewritten to the parameter name
        assert!(rendered.contains("Writes the"));
        assert!(rendered.contains("Hello {a} world"));
    }

    #[test]
    fn ignored_event_errors_generate_unit_returning_method() {
        let e = event(vec![("A", WinType::UInt32)]);
        let codegen = CodegenContext::canonical();
        let specs: Vec<_> = e.params.iter().map(|t| classify(t, &codegen)).collect();
        let p = Provider {
            symbol: "P".into(),
            guid: 0,
            events: vec![],
        };

        let (_, ts) = gen_event_method(
            &e,
            &specs,
            "__write_q",
            &p,
            EventPolicy {
                errors: EventErrors::Ignore,
                input_panics: PanicPolicy {
                    enabled: false,
                    when: None,
                },
                write_panics: PanicPolicy {
                    enabled: false,
                    when: None,
                },
            },
            &codegen,
        )
        .unwrap();
        let rendered = ts.to_string();

        assert!(rendered.contains("pub fn e"));
        assert!(rendered.contains("core :: mem :: drop"));
        assert!(rendered.contains("Errors not configured to panic are ignored"));
    }

    #[test]
    fn panic_policy_distinguishes_input_and_write_errors() {
        let e = event(vec![("A", WinType::UInt32)]);
        let codegen = CodegenContext::canonical();
        let specs: Vec<_> = e.params.iter().map(|t| classify(t, &codegen)).collect();
        let p = Provider {
            symbol: "P".into(),
            guid: 0,
            events: vec![],
        };
        let predicate: Meta = syn::parse_str("debug_assertions").unwrap();

        let (_, ts) = gen_event_method(
            &e,
            &specs,
            "__write_q",
            &p,
            EventPolicy {
                errors: EventErrors::Ignore,
                input_panics: PanicPolicy {
                    enabled: true,
                    when: Some(&predicate),
                },
                write_panics: PanicPolicy {
                    enabled: false,
                    when: None,
                },
            },
            &codegen,
        )
        .unwrap();
        let rendered = ts.to_string();

        assert!(rendered.contains("invalid input for ETW event"));
        assert!(!rendered.contains("failed to write ETW event"));
        assert!(rendered.contains("cfg (debug_assertions)"));
        assert!(rendered.contains("cfg (not (debug_assertions))"));

        let write_predicate: Meta = syn::parse_str(r#"feature = "strict-etw""#).unwrap();
        let (_, write) = gen_event_method(
            &e,
            &specs,
            "__write_q",
            &p,
            EventPolicy {
                errors: EventErrors::Ignore,
                input_panics: PanicPolicy {
                    enabled: false,
                    when: None,
                },
                write_panics: PanicPolicy {
                    enabled: true,
                    when: Some(&write_predicate),
                },
            },
            &codegen,
        )
        .unwrap();
        let rendered = write.to_string();
        assert!(!rendered.contains("invalid input for ETW event"));
        assert!(rendered.contains("failed to write ETW event"));
        assert!(rendered.contains(r#"feature = "strict-etw""#));
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
    fn fieldref_length_keeps_scalar_strings_at_the_declared_width() {
        for (win_type, encoder) in [
            (
                WinType::UnicodeString(Length::FieldRef("NameLength".into())),
                "to_u16cstring_fixed_len",
            ),
            (
                WinType::AnsiString(Length::FieldRef("NameLength".into()), AnsiEncoding::Utf8),
                "to_cstring_fixed_len",
            ),
        ] {
            let e = event(vec![("NameLength", WinType::UInt16), ("Name", win_type)]);
            let plans = plan(&e).unwrap();
            // Unlike a binary length, a string length sets the field width rather than being
            // derived from the value, so it stays a parameter
            assert!(plans[0].param.is_some(), "NameLength should stay exposed");
            assert!(plans[0].temp.is_none());
            assert_eq!(exposed_count(&plans), 2);
            let temp = plans[1]
                .temp
                .as_ref()
                .expect("expected a fixed-length temp")
                .to_string();
            assert!(temp.contains(encoder), "expected {encoder} in {temp}");
            assert!(temp.contains("name_length"), "{temp}");
        }
    }

    #[test]
    fn fieldref_length_checks_caller_encoded_scalar_strings() {
        let e = event(vec![
            ("NameLength", WinType::UInt16),
            (
                "Name",
                WinType::AnsiString(
                    Length::FieldRef("NameLength".into()),
                    AnsiEncoding::ProviderAnsi,
                ),
            ),
        ]);
        let plans = plan(&e).unwrap();
        // Provider-ANSI bytes arrive already encoded, so the width is validated instead
        assert!(plans[1].temp.is_none());
        let validation = plans[1]
            .validation
            .as_ref()
            .expect("expected a length check")
            .to_string();
        assert!(validation.contains("ensure_len"), "{validation}");
        assert!(validation.contains("name_length"), "{validation}");
    }

    #[test]
    fn fieldref_length_on_a_scalar_string_validates_its_target() {
        let e = event(vec![(
            "Name",
            WinType::UnicodeString(Length::FieldRef("Nope".into())),
        )]);
        assert!(plan(&e).is_err(), "unknown length field should be rejected");

        let e = event(vec![
            (
                "Name",
                WinType::UnicodeString(Length::FieldRef("NameLength".into())),
            ),
            ("NameLength", WinType::UInt16),
        ]);
        assert!(plan(&e).is_err(), "forward reference should be rejected");
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
        assert!(plans[0].call.to_string().starts_with("__t"));
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
        assert!(plans[0].call.to_string().starts_with("__t"));
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
        assert!(plans[0].call.to_string().starts_with("__t"));
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
