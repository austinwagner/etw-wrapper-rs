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

use crate::model::{AnsiEncoding, Event, Length, Provider, WinType};
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
            .map(|t| classify(&t.win_type, codegen))
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

/// How a field is surfaced, decided before identifiers are assigned.
#[derive(Clone, Copy)]
enum Kind {
    /// Exposes the field as-is with its helper type.
    Normal,
    /// Exposes fixed-length `win:Binary length="N"` data as `&[u8; N]`.
    FixedBinary(usize),
    /// Exposes `win:UnicodeString` data as `&str` and converts it to a UTF-16 buffer.
    Unicode,
    /// Exposes `win:UnicodeString length="N"` data as `&str`, encoded into exactly N UTF-16 units.
    FixedLenUnicode(usize),
    /// Exposes a provider-code-page `win:AnsiString length="N"` as exact encoded bytes.
    FixedLenProviderAnsi(usize),
    /// Exposes a UTF-8 `win:AnsiString` as `&str` and adds a NUL terminator.
    Utf8Ansi,
    /// Exposes a UTF-8 `win:AnsiString length="N"` as `&str`, encoded into exactly N bytes.
    FixedLenUtf8Ansi(usize),
    /// Hides a length field and derives it from the blob at index `from`.
    DerivedLen { from: usize },
}

/// Returns whether a `WinType` is an integer type that a `win:Binary length="..."` reference may
/// target.
fn is_length_int(w: &WinType) -> bool {
    matches!(
        w,
        WinType::UInt8 | WinType::UInt16 | WinType::UInt32 | WinType::HexInt32
    )
}

/// Decides, for each field of an event, whether it is an exposed parameter, a fixed-length
/// variant, or a length field derived from a later blob, validating `length="field"` refs.
fn plan_event(
    ev: &Event,
    p: &Provider,
    specs: &[FieldSpec],
    codegen: &CodegenContext,
) -> anyhow::Result<Vec<FieldPlan>> {
    let n = ev.params.len();

    // Map original field names to indexes for resolving length="field" references
    let mut index_by_name: HashMap<&str, usize> = HashMap::new();
    for (i, t) in ev.params.iter().enumerate() {
        index_by_name.entry(t.name.as_str()).or_insert(i);
    }

    let mut kinds = vec![Kind::Normal; n];
    // Map each index to the only blob field that may claim it as its length
    let mut claimed_by: Vec<Option<usize>> = vec![None; n];

    for (i, t) in ev.params.iter().enumerate() {
        match &t.win_type {
            // Enforce exact byte counts for fixed-length binary fields at the type level
            WinType::Binary(Length::Constant(len)) => kinds[i] = Kind::FixedBinary(*len as usize),
            // Encode fixed-length Unicode strings to the exact manifest length
            WinType::UnicodeString(Length::Constant(len)) => {
                kinds[i] = Kind::FixedLenUnicode(*len as usize)
            }
            // Convert every other Unicode string to UTF-16 in the generated method
            WinType::UnicodeString(_) => kinds[i] = Kind::Unicode,
            // Default ANSI strings must remain in the provider's code page. For a fixed length,
            // require the caller to provide the exact encoded bytes including the terminator.
            WinType::AnsiString(Length::Constant(len), AnsiEncoding::ProviderAnsi) => {
                kinds[i] = Kind::FixedLenProviderAnsi(*len as usize)
            }
            WinType::AnsiString(_, AnsiEncoding::ProviderAnsi) => {}
            // Output types that explicitly select UTF-8 can safely accept a Rust string.
            WinType::AnsiString(Length::Constant(len), AnsiEncoding::Utf8) => {
                kinds[i] = Kind::FixedLenUtf8Ansi(*len as usize)
            }
            WinType::AnsiString(_, AnsiEncoding::Utf8) => kinds[i] = Kind::Utf8Ansi,
            // Hide and derive the referenced length field for variable-length binary data
            WinType::Binary(Length::FieldRef(name)) => {
                let j = *index_by_name.get(name.as_str()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "binary field {} in event {} of provider {} references unknown length field {:?}",
                        t.name, ev.symbol, p.symbol, name
                    )
                })?;
                if j >= i {
                    anyhow::bail!(
                        "binary field {} in event {} of provider {} references length field {:?}, which must appear before it",
                        t.name,
                        ev.symbol,
                        p.symbol,
                        name
                    );
                }
                if !is_length_int(&ev.params[j].win_type) {
                    anyhow::bail!(
                        "length field {:?} for binary field {} in event {} of provider {} must have type win:UInt8, win:UInt16, win:UInt32, or win:HexInt32",
                        name,
                        t.name,
                        ev.symbol,
                        p.symbol
                    );
                }
                if let Some(other) = claimed_by[j] {
                    anyhow::bail!(
                        "length field {:?} in event {} of provider {} is referenced by multiple fields ({} and {})",
                        name,
                        ev.symbol,
                        p.symbol,
                        ev.params[other].name,
                        t.name
                    );
                }
                claimed_by[j] = Some(i);
                kinds[j] = Kind::DerivedLen { from: i };
            }
            _ => {}
        }
    }

    // Assign identifiers up front so a derived length declared before its blob can name it
    let mut idents: Vec<Ident> = Vec::with_capacity(n);
    let mut exposed_names: HashMap<String, String> = HashMap::new();
    for (i, (t, kind)) in ev.params.iter().zip(&kinds).enumerate() {
        let ident = match kind {
            Kind::DerivedLen { .. } => format_ident!("__len{}", i),
            _ => {
                let name = safe_ident(&ccase!(snake, &t.name));
                if let Some(prev) = exposed_names.insert(name.clone(), t.name.clone()) {
                    anyhow::bail!(
                        "param name {} collides with {} in event {} of provider {} (both transform to: {})",
                        t.name,
                        prev,
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

    let mut plans = Vec::with_capacity(n);
    let runtime = &codegen.runtime;
    for i in 0..n {
        let id = idents[i].clone();
        let plan = match kinds[i] {
            Kind::Normal => {
                let ty = specs[i].rust_ty.clone();
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id.clone(), ty)),
                    temp: None,
                    call: id,
                    doc_ref,
                }
            }
            Kind::FixedBinary(len) => {
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id.clone(), quote!(&[u8; #len]))),
                    temp: None,
                    call: id,
                    doc_ref,
                }
            }
            Kind::Unicode => {
                let tmp = format_ident!("__s{}", i);
                // Borrowing the buffer extends its lifetime to the end of the method body
                let temp = quote! {
                    let #tmp: &[u16] = &#runtime::field::to_u16cstring(#id);
                };
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id, quote!(&str))),
                    temp: Some(temp),
                    call: tmp,
                    doc_ref,
                }
            }
            Kind::FixedLenUnicode(len) => {
                let tmp = format_ident!("__f{}", i);
                let temp = quote! {
                    let #tmp: &[u16] =
                        &#runtime::field::to_u16cstring_fixed_len(#id, #len);
                };
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id, quote!(&str))),
                    temp: Some(temp),
                    call: tmp,
                    doc_ref,
                }
            }
            Kind::FixedLenProviderAnsi(len) => {
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id.clone(), quote!(&[u8; #len]))),
                    temp: None,
                    call: id,
                    doc_ref,
                }
            }
            Kind::Utf8Ansi => {
                let tmp = format_ident!("__a{}", i);
                // Borrowing the buffer extends its lifetime to the end of the method body
                let temp = quote! {
                    let #tmp: &[u8] = &#runtime::field::to_cstring(#id);
                };
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id, quote!(&str))),
                    temp: Some(temp),
                    call: tmp,
                    doc_ref,
                }
            }
            Kind::FixedLenUtf8Ansi(len) => {
                let tmp = format_ident!("__af{}", i);
                let temp = quote! {
                    let #tmp: &[u8] =
                        &#runtime::field::to_cstring_fixed_len(#id, #len);
                };
                let doc_ref = id.to_string();
                FieldPlan {
                    param: Some((id, quote!(&str))),
                    temp: Some(temp),
                    call: tmp,
                    doc_ref,
                }
            }
            Kind::DerivedLen { from } => {
                let int_ty = specs[i].rust_ty.clone();
                let blob = idents[from].clone();
                let temp = quote! {
                    let #id: #int_ty = #runtime::field::checked_len(#blob.len())?;
                };
                // The length field is hidden, a placeholder on it refers to the blob's length
                let doc_ref = format!("{}.len", blob);
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
    /// Borrows a `field::Sid`.
    Sid,
}

struct FieldSpec {
    /// Provides a short fragment used to name the backing helper and deduplicate signatures.
    frag: &'static str,
    rust_ty: TokenStream2,
    class: FieldClass,
}

fn classify(w: &WinType, codegen: &CodegenContext) -> FieldSpec {
    let runtime = &codegen.runtime;
    let scalar = |frag, ty: TokenStream2| FieldSpec {
        frag,
        rust_ty: ty,
        class: FieldClass::Scalar,
    };
    match w {
        WinType::Int8 => scalar("c", quote!(i8)),
        WinType::UInt8 => scalar("u", quote!(u8)),
        WinType::Int16 => scalar("l", quote!(i16)),
        WinType::UInt16 => scalar("h", quote!(u16)),
        WinType::Int32 => scalar("d", quote!(i32)),
        WinType::UInt32 | WinType::HexInt32 => scalar("q", quote!(u32)),
        WinType::Int64 => scalar("i", quote!(i64)),
        WinType::UInt64 | WinType::HexInt64 => scalar("x", quote!(u64)),
        WinType::FileTime => scalar("m", quote!(#runtime::FILETIME)),
        WinType::Float => scalar("f", quote!(f32)),
        WinType::Double => scalar("g", quote!(f64)),
        WinType::Pointer => scalar("p", quote!(usize)),
        WinType::Guid => scalar("j", quote!(#runtime::GUID)),
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
        WinType::SystemTime => scalar("y", quote!(#runtime::SYSTEMTIME)),
        WinType::Sid => FieldSpec {
            frag: "k",
            rust_ty: quote!(&#runtime::field::Sid),
            class: FieldClass::Sid,
        },
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
        let specs: Vec<_> = e
            .params
            .iter()
            .map(|t| classify(&t.win_type, &codegen))
            .collect();
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
        let specs: Vec<_> = e
            .params
            .iter()
            .map(|t| classify(&t.win_type, &codegen))
            .collect();
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
}
