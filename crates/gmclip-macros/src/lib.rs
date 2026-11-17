//! Proc macros (SPEC §3.1, §3.2).
//!
//! - `#[derive(Entity)]` implements `gmclip_model::Entity`: the field table, per-field and
//!   per-map-key canonical-CBOR get/set, the field diff, parent and order key.
//! - `#[command(...)]` on an `impl Command for X { … }` block fills in `const META`
//!   (R-CMD-1 metadata and the schemars schemas) and checks the name at compile time.
//! - `#[wire]` derives serde + schemars for a wire type (specta joins at M2).
#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, LitStr, parse_macro_input};

// ---------------------------------------------------------------------------------------
// #[derive(Entity)]
// ---------------------------------------------------------------------------------------

/// `#[derive(Entity)]`.
///
/// Struct attributes: `#[entity(kind = "part")]` (required; `^[a-z][a-z0-9_]*$`).
/// Field attributes:
/// - `#[entity(id)]`: the `Id<Self>` field. Exactly one; never an op target.
/// - `#[entity(parent)]`: the field whose `AsParent` value names the owning entity
///   (drives the children index, e.g. parts by cut).
/// - `#[entity(order)]`: the `OrderKey` field ordering siblings under the parent.
/// - `#[entity(map)]`: a `BTreeMap<K, V>` whose ops address one key at a time (SPEC §3.2).
#[proc_macro_derive(Entity, attributes(entity))]
pub fn derive_entity(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match entity_impl(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

#[derive(Default)]
struct FieldFlags {
    id: bool,
    parent: bool,
    order: bool,
    map: bool,
}

fn field_flags(attrs: &[syn::Attribute]) -> syn::Result<FieldFlags> {
    let mut f = FieldFlags::default();
    for a in attrs.iter().filter(|a| a.path().is_ident("entity")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("id") {
                f.id = true;
            } else if m.path.is_ident("parent") {
                f.parent = true;
            } else if m.path.is_ident("order") {
                f.order = true;
            } else if m.path.is_ident("map") {
                f.map = true;
            } else {
                return Err(m.error("expected one of: id, parent, order, map"));
            }
            Ok(())
        })?;
    }
    Ok(f)
}

fn valid_kind(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn entity_impl(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &input.ident;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "Entity types are not generic",
        ));
    }
    let mut kind: Option<LitStr> = None;
    for a in input.attrs.iter().filter(|a| a.path().is_ident("entity")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("kind") {
                kind = Some(m.value()?.parse()?);
                Ok(())
            } else {
                Err(m.error("expected `kind = \"...\"`"))
            }
        })?;
    }
    let kind = kind
        .ok_or_else(|| syn::Error::new(Span::call_site(), "missing #[entity(kind = \"...\")]"))?;
    if !valid_kind(&kind.value()) {
        return Err(syn::Error::new(
            kind.span(),
            "entity kind must match ^[a-z][a-z0-9_]{0,63}$",
        ));
    }
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(n) => &n.named,
            _ => {
                return Err(syn::Error::new(
                    s.fields.span(),
                    "Entity needs named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                input.span(),
                "Entity can only be derived for structs",
            ));
        }
    };

    let rt = quote!(::gmclip_model::entity::rt);
    let m = quote!(::gmclip_model);
    let mut id_field = None;
    let mut parent_field = None;
    let mut order_field = None;
    let mut descs = Vec::new();
    let mut get_arms = Vec::new();
    let mut set_arms = Vec::new();
    let mut diffs = Vec::new();
    let mut n: u16 = 0;
    for f in fields {
        let ident = f.ident.as_ref().expect("named");
        let flags = field_flags(&f.attrs)?;
        if flags.id {
            if id_field.replace(ident.clone()).is_some() {
                return Err(syn::Error::new(f.span(), "two #[entity(id)] fields"));
            }
            if flags.map || flags.parent || flags.order {
                return Err(syn::Error::new(
                    f.span(),
                    "#[entity(id)] cannot be combined",
                ));
            }
            continue;
        }
        if flags.parent && parent_field.replace(ident.clone()).is_some() {
            return Err(syn::Error::new(f.span(), "two #[entity(parent)] fields"));
        }
        if flags.order && order_field.replace(ident.clone()).is_some() {
            return Err(syn::Error::new(f.span(), "two #[entity(order)] fields"));
        }
        let fname = ident.to_string();
        let fname = fname.strip_prefix("r#").unwrap_or(&fname).to_owned();
        let map = flags.map;
        let idx = n;
        n = n
            .checked_add(1)
            .ok_or_else(|| syn::Error::new(f.span(), "too many fields"))?;
        descs.push(quote! {
            #m::FieldDesc { id: #m::FieldId(#idx), name: #fname, map: #map }
        });
        if map {
            get_arms.push(quote! {
                #idx => match key { Some(k) => #rt::map_get(&self.#ident, k), None => Err(#rt::needs_key(#fname)) }
            });
            set_arms.push(quote! {
                #idx => match key { Some(k) => #rt::map_set(&mut self.#ident, k, v), None => Err(#rt::needs_key(#fname)) }
            });
            diffs.push(
                quote! { #rt::map_diff(out, #m::FieldId(#idx), &self.#ident, &newer.#ident); },
            );
        } else {
            get_arms.push(quote! {
                #idx => match key { None => Ok(#rt::encode(&self.#ident)), Some(_) => Err(#rt::no_key(#fname)) }
            });
            set_arms.push(quote! {
                #idx => match key { None => { self.#ident = #rt::decode(v)?; Ok(()) }, Some(_) => Err(#rt::no_key(#fname)) }
            });
            diffs.push(
                quote! { #rt::field_diff(out, #m::FieldId(#idx), &self.#ident, &newer.#ident); },
            );
        }
    }
    let id_field =
        id_field.ok_or_else(|| syn::Error::new(input.span(), "missing #[entity(id)] field"))?;
    let parent = match &parent_field {
        Some(p) => quote!(#m::entity::AsParent::as_parent(&self.#p)),
        None => quote!(None),
    };
    let order = match &order_field {
        Some(o) => quote!(Some(&self.#o)),
        None => quote!(None),
    };
    let ordered = order_field.is_some();
    let consts = format_ident!("__{}_FIELDS", name.to_string().to_uppercase());
    Ok(quote! {
        #[allow(non_upper_case_globals)]
        const #consts: &[#m::FieldDesc] = &[#(#descs),*];

        impl #m::Entity for #name {
            const KIND: #m::EntityKind = #m::EntityKind(#kind);
            const ORDERED: bool = #ordered;

            fn id(&self) -> #m::Id<Self> {
                self.#id_field
            }

            fn fields() -> &'static [#m::FieldDesc] {
                #consts
            }

            fn get_field(&self, path: &#m::FieldPath) -> Result<#m::FieldValue, #m::ModelError> {
                let key = path.key.as_deref();
                match path.field.0 {
                    #(#get_arms,)*
                    _ => Err(#m::ModelError::UnknownField(path.field)),
                }
            }

            fn set_field(&mut self, path: &#m::FieldPath, v: &#m::FieldValue) -> Result<(), #m::ModelError> {
                let key = path.key.as_deref();
                match path.field.0 {
                    #(#set_arms,)*
                    _ => Err(#m::ModelError::UnknownField(path.field)),
                }
            }

            fn diff(&self, newer: &Self, out: &mut Vec<#m::entity::FieldChange>) {
                #(#diffs)*
            }

            fn parent(&self) -> Option<#m::EntityId> {
                #parent
            }

            fn order_key(&self) -> Option<&#m::OrderKey> {
                #order
            }
        }
    })
}

// ---------------------------------------------------------------------------------------
// #[command(...)]
// ---------------------------------------------------------------------------------------

struct CommandArgs {
    name: LitStr,
    version: u16,
    since: String,
    scope: syn::Ident,
    exec: syn::Ident,
    undo: syn::Ident,
    ui: Option<(LitStr, LitStr)>,
    doc: LitStr,
    examples: Vec<LitStr>,
}

fn camel(s: &str) -> String {
    s.split('_')
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect(),
                None => String::new(),
            }
        })
        .collect()
}

fn valid_command_name(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn parse_command_args(attr: TokenStream) -> syn::Result<CommandArgs> {
    let mut name = None;
    let mut version = 1u16;
    let mut since = "M1".to_owned();
    let mut scope = None;
    let mut exec = "sync".to_owned();
    let mut undo = None;
    let mut ui = None;
    let mut doc = None;
    let mut examples = Vec::new();
    let parser = syn::meta::parser(|m| {
        let lit = |m: &syn::meta::ParseNestedMeta| -> syn::Result<LitStr> { m.value()?.parse() };
        if m.path.is_ident("name") {
            name = Some(lit(&m)?);
        } else if m.path.is_ident("version") {
            let v: syn::LitInt = m.value()?.parse()?;
            version = v.base10_parse()?;
        } else if m.path.is_ident("since") {
            since = lit(&m)?.value();
        } else if m.path.is_ident("scope") {
            scope = Some(lit(&m)?);
        } else if m.path.is_ident("exec") {
            exec = lit(&m)?.value();
        } else if m.path.is_ident("undo") {
            undo = Some(lit(&m)?);
        } else if m.path.is_ident("doc") {
            doc = Some(lit(&m)?);
        } else if m.path.is_ident("example") {
            examples.push(lit(&m)?);
        } else if m.path.is_ident("ui") {
            let mut label = None;
            let mut menu = None;
            m.parse_nested_meta(|u| {
                if u.path.is_ident("label") {
                    label = Some(u.value()?.parse()?);
                } else if u.path.is_ident("menu") {
                    menu = Some(u.value()?.parse()?);
                } else {
                    return Err(u.error("expected label or menu"));
                }
                Ok(())
            })?;
            match (label, menu) {
                (Some(l), Some(mn)) => ui = Some((l, mn)),
                _ => return Err(m.error("ui(...) needs label and menu")),
            }
        } else {
            return Err(m.error(
                "expected name, version, since, scope, exec, undo, ui(...), doc or example",
            ));
        }
        Ok(())
    });
    syn::parse::Parser::parse(parser, attr)?;
    let name = name.ok_or_else(|| syn::Error::new(Span::call_site(), "missing name = \"…\""))?;
    if !valid_command_name(&name.value()) {
        return Err(syn::Error::new(
            name.span(),
            "command names match ^[a-z0-9_]{1,64}$",
        ));
    }
    let doc = doc.ok_or_else(|| syn::Error::new(Span::call_site(), "missing doc = \"…\""))?;
    if doc.value().trim().is_empty() {
        return Err(syn::Error::new(
            doc.span(),
            "doc must not be empty (R-CMD-1)",
        ));
    }
    if examples.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "at least one example = \"…\" (R-CMD-1)",
        ));
    }
    let pick = |v: Option<LitStr>, what: &str, allowed: &[&str]| -> syn::Result<syn::Ident> {
        let v = v.ok_or_else(|| syn::Error::new(Span::call_site(), format!("missing {what}")))?;
        if !allowed.contains(&v.value().as_str()) {
            return Err(syn::Error::new(
                v.span(),
                format!("{what} must be one of {allowed:?}"),
            ));
        }
        Ok(syn::Ident::new(&camel(&v.value()), v.span()))
    };
    let scope = pick(
        scope,
        "scope",
        &["read", "read_media", "edit", "render", "ai", "admin"],
    )?;
    let undo = pick(undo, "undo", &["undoable", "read_only", "destructive"])?;
    let exec = pick(
        Some(LitStr::new(&exec, Span::call_site())),
        "exec",
        &["sync", "job"],
    )?;
    Ok(CommandArgs {
        name,
        version,
        since,
        scope,
        exec,
        undo,
        ui,
        doc,
        examples,
    })
}

/// `#[command(name = "part_trim", scope = "edit", undo = "undoable", doc = "…",
/// example = "{…}")]` on `impl Command for PartTrim { type Args = …; type Output = …;
/// fn run(…) … }` fills in `const META`. Optional: `version = 1`, `since = "M1"`,
/// `exec = "sync"|"job"`, `ui(label = "…", menu = "…")` (default `Hidden`).
#[proc_macro_attribute]
pub fn command(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match parse_command_args(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let mut imp = parse_macro_input!(item as syn::ItemImpl);
    if imp.trait_.is_none() {
        return syn::Error::new(imp.span(), "#[command] goes on `impl Command for X`")
            .to_compile_error()
            .into();
    }
    let c = quote!(::gmclip_core::command);
    let CommandArgs {
        name,
        version,
        since,
        scope,
        exec,
        undo,
        ui,
        doc,
        examples,
    } = args;
    let ui = match ui {
        None => quote!(#c::UiMeta::Hidden),
        Some((label, menu)) => quote!(#c::UiMeta::Action { label: #label, menu: #menu }),
    };
    let meta: syn::ImplItem = syn::parse_quote! {
        const META: #c::CommandMeta = #c::CommandMeta {
            name: #name,
            version: #version,
            since: #since,
            scope: #c::Scope::#scope,
            exec: #c::Exec::#exec,
            undo: #c::UndoClass::#undo,
            ui: #ui,
            doc: #doc,
            examples: &[#(#examples),*],
            args_schema: || ::schemars::schema_for!(<Self as #c::Command>::Args),
            output_schema: || ::schemars::schema_for!(<Self as #c::Command>::Output),
        };
    };
    imp.items.insert(0, meta);
    quote!(#imp).into()
}

/// `#[wire]`: derives `Serialize`, `Deserialize` and `JsonSchema` for a wire type.
/// (specta joins at M2, SPEC §3.2 Codegen.)
#[proc_macro_attribute]
pub fn wire(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = TokenStream2::from(item);
    quote! {
        #[derive(::serde::Serialize, ::serde::Deserialize, ::schemars::JsonSchema)]
        #item
    }
    .into()
}
