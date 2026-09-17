//! Derive macro for the `Tool` trait from `flux-core`.
//!
//! ## Input
//!
//! ```ignore
//! use flux_core::Tool;
//! use flux_core::CoreError;
//!
//! #[derive(Tool, ::serde::Deserialize)]
//! #[tool(
//!     name = "read_file",
//!     description = "Read contents of a file",
//! )]
//! struct ReadFile {
//!     /// Path to the file (resolved against the chat boundary via `ctx.resolve`).
//!     file_path: String,
//!     /// Line to start reading from.
//!     offset: Option<usize>,
//! }
//!
//! impl ReadFile {
//!     async fn execute(&self, ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
//!         // ... resolve the path argument against the boundary, then read
//!         let path = ctx.resolve(&self.file_path)?;
//!         Ok(std::fs::read_to_string(path)?)
//!     }
//! }
//! ```
//!
//! ## Expansion
//!
//! The macro generates the `Tool` impl only (the `Deserialize` impl comes
//! from the separate serde derive) — roughly:
//!
//! ```ignore
//! #[automatically_derived]
//! #[::async_trait::async_trait]
//! impl ::flux_core::Tool for ReadFile {
//!     fn name(&self) -> &str {
//!         "read_file"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "Read contents of a file"
//!     }
//!
//!     fn schema(&self) -> ::serde_json::Value {
//!         ::serde_json::json!({
//!             "type": "object",
//!             "properties": {
//!                 "file_path": {
//!                     "type": "string",
//!                     "description": "Path to the file.",
//!                 },
//!                 "offset": {
//!                     "type": "integer",
//!                     "description": "Line to start reading from.",
//!                 },
//!             },
//!             "required": ["file_path"],
//!             "additionalProperties": false,
//!         })
//!     }
//!
//!     async fn call(
//!         &self,
//!         arguments: ::std::collections::HashMap<::std::string::String, ::serde_json::Value>,
//!         ctx: ::flux_core::ToolCtx,
//!     ) -> ::std::result::Result<::std::string::String, ::flux_core::CoreError> {
//!         let this: Self = ::serde_json::from_value(::serde_json::Value::Object(
//!             ::serde_json::Map::from_iter(arguments),
//!         ))
//!         .map_err(|e| ::flux_core::CoreError::InvalidArguments(e.to_string()))?;
//!         this.execute(ctx).await
//!     }
//! }
//! ```
//!
//! Notes:
//!
//! - `schema()` is inferred from the struct fields: doc comments become
//!   descriptions, `Option<T>` fields are optional, and `#[tool(skip)]`
//!   excludes a field from the schema.
//! - `Vec<T>` fields become `{"type": "array", "items": ...}`. Primitive
//!   item types (`String`/`bool`/integers/floats, optionally
//!   `Option`-wrapped) map to a `{"type": <name>}` item schema; any other
//!   item type delegates to `<T>::item_schema()` — the item struct carries
//!   `#[derive(ToolItem)]` (see below) and the build fails without it.
//! - `call()` converts the raw `HashMap<String, Value>` arguments to a JSON
//!   object, deserializes into `Self` (failures become
//!   `CoreError::InvalidArguments`) and delegates to `self.execute(ctx)`, which
//!   the user must define. Long-running tools should honor `ctx.cancel`
//!   (stop promptly on cancellation, return partial output) — a tool that
//!   ignores it is force-terminated by the kernel after a grace period.
//! - There is no approval/preprocessing layer (no-allowlist trust model): the
//!   chat boundary rides the invocation context — resolve path arguments
//!   through [`::flux_core::ToolCtx::resolve`], never by trusting raw
//!   LLM-supplied locations. Tools never touch the state store.
//!
//! ## Nested items: `#[derive(ToolItem)]`
//!
//! An array-of-objects parameter needs an object schema for its items:
//!
//! ```ignore
//! #[derive(ToolItem, ::serde::Deserialize)]
//! struct EditItem {
//!     /// Path to the file to edit.
//!     file_path: String,
//!     /// Replacement text.
//!     new_string: String,
//! }
//!
//! #[derive(Tool, ::serde::Deserialize)]
//! #[tool(name = "edit_files", description = "...")]
//! struct EditFilesTool {
//!     /// Edits to apply.
//!     edits: Vec<EditItem>,
//! }
//! ```
//!
//! `ToolItem` generates `EditItem::item_schema()` — the object schema built
//! from the item's fields under the same rules as `Tool` (doc comments as
//! descriptions, `Option<T>` optional, `#[tool(skip)]`/`#[tool(required)]`).
//! The `Tool` derive references it for `Vec<EditItem>` fields; item structs
//! must also derive `Deserialize`, and `#[serde(default)]` is unsupported on
//! item fields (optionality is `Option<T>` only — see the derive's docs).

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse_macro_input};

/// Derive macro for the `Tool` trait.
///
/// # Struct-level attributes
///
/// - `#[tool(name = "...")]` — required, the tool name.
/// - `#[tool(description = "...")]` — required, the tool description.
///
/// # Field-level
///
/// - Doc comments (`/// ...`) are used as field descriptions in the schema.
/// - `#[tool(skip)]` excludes a field from the schema entirely; the
///   deserialized struct then requires the field to be absent (pair with
///   `#[serde(default)]` for skip fields the model may still send).
/// - `#[tool(required)]` forces an `Option<T>` field into `required`.
///
/// # Schema inference
///
/// `bool` → `boolean`; integer types → `integer`; floats → `number`;
/// `Vec<T>` → `array` with `items` typed from `T`; `Option<T>` is optional
/// (not listed in `required`); any other type falls back to `string`.
/// `#[serde(default)]` fields stay out of `required`. The generated schema
/// always sets `"additionalProperties": false`.
#[proc_macro_derive(Tool, attributes(tool))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_tool_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn derive_tool_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_name = &input.ident;
    let fields = match &input.data {
        syn::Data::Struct(s) => &s.fields,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "Tool can only be derived for structs",
            ));
        }
    };

    validate_tool_attr_keys(&input.attrs, STRUCT_TOOL_KEYS, "a struct")?;
    for field in fields {
        validate_tool_attr_keys(&field.attrs, FIELD_TOOL_KEYS, "a field")?;
    }

    let tool_name = parse_attr_str(&input.attrs, "name")?;
    let tool_desc = parse_attr_str(&input.attrs, "description")?;

    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let (schema_fields, required_fields) = build_schema_fields(fields);
    let required = if required_fields.is_empty() {
        quote! { "required": [], }
    } else {
        quote! { "required": [ #(#required_fields),* ], }
    };
    let schema_body = quote! {
        fn schema(&self) -> ::serde_json::Value {
            ::serde_json::json!({
                "type": "object",
                "properties": {
                    #(#schema_fields),*
                },
                #required
                "additionalProperties": false,
            })
        }
    };

    let expanded = quote! {
        #[automatically_derived]
        #[::async_trait::async_trait]
        impl #impl_generics ::flux_core::Tool for #struct_name #ty_generics #where_clause {
            fn name(&self) -> &str {
                #tool_name
            }

            fn description(&self) -> &str {
                #tool_desc
            }

            #schema_body

            async fn call(
                &self,
                arguments: ::std::collections::HashMap<::std::string::String, ::serde_json::Value>,
                ctx: ::flux_core::ToolCtx,
            ) -> ::std::result::Result<::std::string::String, ::flux_core::CoreError> {
                let this: Self = ::serde_json::from_value(::serde_json::Value::Object(
                    ::serde_json::Map::from_iter(arguments),
                ))
                .map_err(|e| ::flux_core::CoreError::InvalidArguments(e.to_string()))?;
                this.execute(ctx).await
            }
        }
    };

    Ok(expanded)
}

/// Derive macro for nested tool-parameter item structs.
///
/// Generates an inherent `pub fn item_schema() -> ::serde_json::Value` — the
/// JSON object schema an enclosing `#[derive(Tool)]` struct emits for
/// `Vec<Item>` fields (its array inference calls `<Item>::item_schema()` for
/// non-primitive item types; a missing derive fails compilation).
///
/// Field rules mirror `Tool`: doc comments become descriptions, `Option<T>`
/// fields are optional, `#[tool(skip)]` excludes a field, `#[tool(required)]`
/// forces an `Option<T>` into `required`, and the object always sets
/// `"additionalProperties": false`.
///
/// The item struct must also derive `Deserialize` (the enclosing tool's
/// `call()` deserializes the whole argument object). `#[serde(default)]` is
/// not supported on item fields — express optionality with `Option<T>` only,
/// so the schema's `required` list can never be looser than the runtime.
#[proc_macro_derive(ToolItem, attributes(tool))]
pub fn derive_tool_item(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match derive_tool_item_impl(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn derive_tool_item_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let struct_name = &input.ident;
    let fields = match &input.data {
        syn::Data::Struct(s) => &s.fields,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "ToolItem can only be derived for structs",
            ));
        }
    };
    for field in fields {
        validate_tool_attr_keys(&field.attrs, FIELD_TOOL_KEYS, "a field")?;
    }

    let (schema_fields, required_fields) = build_schema_fields(fields);
    let required = if required_fields.is_empty() {
        quote! { "required": [], }
    } else {
        quote! { "required": [ #(#required_fields),* ], }
    };
    Ok(quote! {
        #[automatically_derived]
        impl #struct_name {
            pub fn item_schema() -> ::serde_json::Value {
                ::serde_json::json!({
                    "type": "object",
                    "properties": {
                        #(#schema_fields),*
                    },
                    #required
                    "additionalProperties": false,
                })
            }
        }
    })
}

/// Extract a required string attribute from `#[tool(name = "...", ...)]`.
fn parse_attr_str(attrs: &[syn::Attribute], key: &str) -> syn::Result<String> {
    parse_attr_str_opt(attrs, key).ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("missing required attribute #[tool({key} = \"...\")]"),
        )
    })
}

/// Allowed `#[tool(...)]` keys on the struct.
const STRUCT_TOOL_KEYS: &[&str] = &["name", "description"];
/// Allowed `#[tool(...)]` keys on a field.
const FIELD_TOOL_KEYS: &[&str] = &["skip", "required"];

/// Reject any `#[tool(...)]` key that is not recognized in the given context.
///
/// Struct-level keys (`name`/`description`/`state`) and field-level keys
/// (`skip`/`required`) are validated separately so a key valid in one context
/// is still an error in the other.
fn validate_tool_attr_keys(
    attrs: &[syn::Attribute],
    allowed: &[&str],
    context: &str,
) -> syn::Result<()> {
    for attr in attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        if let syn::Meta::List(list) = &attr.meta
            && let Ok(nested) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
        {
            for meta in nested {
                let Some(key) = meta.path().get_ident().map(|i| i.to_string()) else {
                    continue;
                };
                if !allowed.contains(&key.as_str()) {
                    return Err(syn::Error::new_spanned(
                        &meta,
                        format!(
                            "unknown #[tool({key})] attribute on {context} — expected one of: {}",
                            allowed.join(", ")
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Build schema property tokens and collect required field names.
///
/// Returns `(property_tokens, required_field_literals)`.
fn build_schema_fields(fields: &syn::Fields) -> (Vec<proc_macro2::TokenStream>, Vec<syn::LitStr>) {
    let mut entries = Vec::new();
    let mut required = Vec::new();

    for field in fields {
        let field_name = field
            .ident
            .as_ref()
            .map(|i| i.to_string())
            .unwrap_or_default();

        // Support #[tool(skip)] to exclude a field from the schema.
        if has_tool_attr(&field.attrs, "skip") {
            continue;
        }

        // Extract doc comments.
        let description = field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc"))
            .filter_map(|a| {
                if let syn::Meta::NameValue(nv) = &a.meta
                    && let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = &nv.value
                {
                    return Some(s.value().trim().to_string());
                }
                None
            })
            .reduce(|a, b| format!("{a} {b}"))
            .unwrap_or_default();

        let (json_type, is_optional) = infer_json_type(&field.ty);
        let force_required = has_tool_attr(&field.attrs, "required");
        let serde_default = field.attrs.iter().any(has_serde_default);

        let field_lit = syn::LitStr::new(&field_name, proc_macro2::Span::call_site());
        // `#[serde(default)]` fields stay out of `required` — the model is
        // never forced to send a value the tool would default anyway.
        if (!is_optional || force_required) && !serde_default {
            required.push(field_lit.clone());
        }

        let type_token = if json_type == "array" {
            let items = array_items_expr(&field.ty);
            quote! {
                ::serde_json::json!({
                    "type": "array",
                    "items": #items,
                    "description": #description,
                })
            }
        } else {
            quote! {
                ::serde_json::json!({
                    "type": #json_type,
                    "description": #description,
                })
            }
        };

        entries.push(quote! {
            #field_lit: #type_token
        });
    }

    (entries, required)
}

/// Maps a Rust type to `(json_schema_type, is_optional)`.
fn infer_json_type(ty: &syn::Type) -> (String, bool) {
    let path = match ty {
        syn::Type::Path(p) => &p.path,
        _ => return ("string".to_string(), false),
    };
    let last_seg = path.segments.last();
    let last = last_seg.map(|s| s.ident.to_string()).unwrap_or_default();

    // Check for Option<T> — unwrap and mark as optional.
    if last == "Option" {
        if let Some(seg) = last_seg
            && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
            && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
        {
            let (inner_type, _) = infer_json_type(inner);
            return (inner_type, true);
        }
        return ("string".to_string(), true);
    }

    // Check for Vec<T> — return "array".
    if last == "Vec" {
        return ("array".to_string(), false);
    }

    let json_type = match last.as_str() {
        "bool" => "boolean".to_string(),
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => {
            "integer".to_string()
        }
        "f32" | "f64" => "number".to_string(),
        _ => "string".to_string(),
    };
    (json_type, false)
}

/// The element type of an array field, looking through `Option<...>` layers
/// (`Vec<T>` → `T`, `Option<Vec<T>>` → `T`); `None` when the type is not an
/// array under those wrappers.
fn vec_item_type(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    let seg = p.path.segments.last()?;
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    let syn::GenericArgument::Type(inner) = args.args.first()? else {
        return None;
    };
    match seg.ident.to_string().as_str() {
        "Vec" => Some(inner),
        "Option" => vec_item_type(inner),
        _ => None,
    }
}

/// JSON type name for a PRIMITIVE array item — `bool`/integers/floats/
/// `String`, optionally `Option`-wrapped (mirrors `infer_json_type`'s
/// fallback semantics). `None` for any other path type.
fn primitive_json_type(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    let seg = p.path.segments.last()?;
    let name = seg.ident.to_string();
    if name == "Option" {
        let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
            return None;
        };
        let syn::GenericArgument::Type(inner) = args.args.first()? else {
            return None;
        };
        return primitive_json_type(inner);
    }
    match name.as_str() {
        "bool" => Some("boolean".into()),
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize" => {
            Some("integer".into())
        }
        "f32" | "f64" => Some("number".into()),
        "String" => Some("string".into()),
        _ => None,
    }
}

/// The JSON expression for an array field's `"items"` value: a primitive
/// item type maps to `{ "type": <name> }`; any other path type to
/// `(<item>::item_schema())` — the item struct must `#[derive(ToolItem)]`
/// or compilation fails (a wrong schema fails loudly, not silently).
fn array_items_expr(ty: &syn::Type) -> proc_macro2::TokenStream {
    match vec_item_type(ty) {
        Some(item) => match primitive_json_type(item) {
            Some(t) => quote! { { "type": #t } },
            None => quote! { (#item::item_schema()) },
        },
        // Unreachable for fields infer_json_type already typed "array";
        // the legacy fallback keeps that path harmless.
        None => quote! { { "type": "string" } },
    }
}

/// True when a `#[serde(...)]` attribute carries a `default` meta item —
/// `#[serde(default)]` (bare path) or `#[serde(default = "path")]`
/// (name-value). Parsed structurally: substring matching on token text
/// would misread e.g. `rename = "default_name"` and silently flip the
/// field out of `required`. A malformed list counts as no-default — the
/// field stays required (fail-safe direction).
fn has_serde_default(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("serde") {
        return false;
    }
    let syn::Meta::List(list) = &attr.meta else {
        return false;
    };
    list.parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map(|metas| metas.iter().any(|m| m.path().is_ident("default")))
        .unwrap_or(false)
}

/// Check whether a field has a specific `#[tool(...)]` flag attribute.
fn has_tool_attr(attrs: &[syn::Attribute], key: &str) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        if let syn::Meta::List(list) = &attr.meta
            && let Ok(nested) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
        {
            for meta in nested {
                if meta.path().is_ident(key) {
                    return true;
                }
            }
        }
    }
    false
}

/// Extract an optional string attribute from `#[tool(...)]`.
fn parse_attr_str_opt(attrs: &[syn::Attribute], key: &str) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("tool") {
            continue;
        }
        if let syn::Meta::List(list) = &attr.meta
            && let Ok(nested) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
        {
            for meta in nested {
                if let syn::Meta::NameValue(nv) = meta
                    && nv.path.is_ident(key)
                    && let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = &nv.value
                {
                    return Some(s.value());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_keys_accept_known() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[tool(name = "a", description = "b")])];
        assert!(validate_tool_attr_keys(&attrs, STRUCT_TOOL_KEYS, "a struct").is_ok());
    }

    #[test]
    fn struct_keys_reject_unknown() {
        let attrs: Vec<syn::Attribute> =
            vec![syn::parse_quote!(#[tool(name = "a", schema = "{}")])];
        let err = validate_tool_attr_keys(&attrs, STRUCT_TOOL_KEYS, "a struct").unwrap_err();
        assert!(err.to_string().contains("schema"), "got: {err}");
        assert!(err.to_string().contains("name"), "got: {err}");
    }

    #[test]
    fn field_keys_accept_known() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[tool(skip)])];
        assert!(validate_tool_attr_keys(&attrs, FIELD_TOOL_KEYS, "a field").is_ok());
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[tool(required)])];
        assert!(validate_tool_attr_keys(&attrs, FIELD_TOOL_KEYS, "a field").is_ok());
    }

    #[test]
    fn field_keys_reject_unknown() {
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[tool(skip, extra)])];
        let err = validate_tool_attr_keys(&attrs, FIELD_TOOL_KEYS, "a field").unwrap_err();
        assert!(err.to_string().contains("extra"), "got: {err}");
        // `state` was removed with the field-level state mechanism.
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[tool(state = "workdir")])];
        let err = validate_tool_attr_keys(&attrs, FIELD_TOOL_KEYS, "a field").unwrap_err();
        assert!(err.to_string().contains("state"), "got: {err}");
    }

    #[test]
    fn struct_keys_do_not_leak_into_fields_and_vice_versa() {
        // `skip` is field-only; `required` is field-only. Each context
        // rejects the other's keys.
        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[tool(skip)])];
        assert!(validate_tool_attr_keys(&attrs, STRUCT_TOOL_KEYS, "a struct").is_err());

        let attrs: Vec<syn::Attribute> = vec![syn::parse_quote!(#[tool(required)])];
        assert!(validate_tool_attr_keys(&attrs, STRUCT_TOOL_KEYS, "a struct").is_err());
    }
}
