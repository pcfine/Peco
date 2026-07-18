// ============================================================================
// peco-derive — #[peco_tool] proc-macro attribute
// ============================================================================
//
// Generates a zero-sized struct that implements `peco_core::tools::Tool`.
// Depends only on `peco-core`.
//
// ## Usage
//
// ```ignore
// use peco_derive::peco_tool;
// use peco_core::tools::ToolError;
//
// #[peco_tool(
//     name = "my_tool",
//     description = "Does something useful",
//     params(
//         arg1 = "First argument description",
//         arg2 = "Second argument description",
//     )
// )]
// async fn my_tool(arg1: String, arg2: Option<i32>) -> Result<String, ToolError> {
//     Ok(format!("{arg1} {arg2:?}"))
// }
// ```

extern crate proc_macro;

use convert_case::{Case, Casing};
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use std::collections::HashMap;
use syn::{
    Attribute, Ident, LitStr, ReturnType, Token, Type, parse_macro_input, punctuated::Punctuated,
};

// ── crate 路径解析 ─────────────────────────────────────────────────────────────

/// Resolve the path to `peco-core` in the calling crate's dependency graph.
///
/// - Inside `peco-core` itself → `crate`
/// - In a downstream crate with `peco-core` as dep → `::peco_core`
/// - Not found → `::peco_core` (fallback)
fn peco_core_path() -> proc_macro2::TokenStream {
    match proc_macro_crate::crate_name("peco-core") {
        Ok(proc_macro_crate::FoundCrate::Itself) => quote!(crate),
        Ok(proc_macro_crate::FoundCrate::Name(name)) => {
            let ident = format_ident!("{name}");
            quote!(::#ident)
        }
        Err(_) => quote!(::peco_core),
    }
}

// ── MacroArgs ─────────────────────────────────────────────────────────────────

/// Parsed attribute arguments for `#[peco_tool(...)]`.
struct MacroArgs {
    name: Option<String>,
    description: Option<String>,
    param_descriptions: HashMap<String, String>,
    required: Option<Vec<String>>,
}

impl syn::parse::Parse for MacroArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut description = None;
        let mut param_descriptions = HashMap::new();
        let mut required = None;

        // Parse comma-separated key-value pairs
        let pairs: Punctuated<MacroArgPair, Token![,]> =
            input.parse_terminated(MacroArgPair::parse, Token![,])?;

        for pair in pairs {
            match pair {
                MacroArgPair::Name(s) => name = Some(s),
                MacroArgPair::Description(s) => description = Some(s),
                MacroArgPair::Params(map) => param_descriptions = map,
                MacroArgPair::Required(v) => required = Some(v),
            }
        }

        Ok(MacroArgs {
            name,
            description,
            param_descriptions,
            required,
        })
    }
}

/// A single key-value argument in the macro attribute.
enum MacroArgPair {
    Name(String),
    Description(String),
    Params(HashMap<String, String>),
    Required(Vec<String>),
}

impl syn::parse::Parse for MacroArgPair {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let ident: Ident = input.parse()?;
        let ident_str = ident.to_string();

        match ident_str.as_str() {
            "name" => {
                input.parse::<Token![=]>()?;
                let lit: LitStr = input.parse()?;
                Ok(MacroArgPair::Name(lit.value()))
            }
            "description" => {
                input.parse::<Token![=]>()?;
                let lit: LitStr = input.parse()?;
                Ok(MacroArgPair::Description(lit.value()))
            }
            "params" => {
                let group_content;
                syn::parenthesized!(group_content in input);
                // Parse comma-separated key = "value" pairs inside params(...)
                let pairs: Punctuated<ParamEntry, Token![,]> =
                    group_content.parse_terminated(ParamEntry::parse, Token![,])?;
                let map: HashMap<String, String> =
                    pairs.into_iter().map(|e| (e.key, e.value)).collect();
                Ok(MacroArgPair::Params(map))
            }
            "required" => {
                input.parse::<Token![=]>()?;
                let group_content;
                syn::bracketed!(group_content in input);
                let strings: Punctuated<LitStr, Token![,]> =
                    group_content.parse_terminated(|s| s.parse(), Token![,])?;
                let vec: Vec<String> = strings.iter().map(|l| l.value()).collect();
                Ok(MacroArgPair::Required(vec))
            }
            other => Err(syn::Error::new(
                ident.span(),
                format!(
                    "unexpected argument `{other}`; expected one of: name, description, params, required"
                ),
            )),
        }
    }
}

/// A single `key = "value"` entry inside `params(...)`.
struct ParamEntry {
    key: String,
    value: String,
}

impl syn::parse::Parse for ParamEntry {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        input.parse::<Token![=]>()?;
        let value: LitStr = input.parse()?;
        Ok(ParamEntry {
            key: key.to_string(),
            value: value.value(),
        })
    }
}

// ── 辅助函数 ─────────────────────────────────────────────────────────────────

/// Extract the first doc comment (`/// ...`) from a list of attributes.
fn extract_doc_comment(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("doc") {
            if let syn::Meta::NameValue(meta_nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(lit_str),
                    ..
                }) = &meta_nv.value
                {
                    let doc = lit_str.value();
                    let trimmed = doc.trim().to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
    }
    None
}

/// Check if a type is `Option<T>`.
fn is_option_type(ty: &Type) -> bool {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            return segment.ident == "Option";
        }
    }
    false
}

/// Validate tool name: only lowercase letters, digits, hyphens, and underscores.
fn validate_tool_name(name: &str) -> syn::Result<()> {
    for ch in name.chars() {
        if !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && ch != '-' && ch != '_' {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("invalid tool name `{name}`: allowed chars are a-z, 0-9, `-`, `_`"),
            ));
        }
    }
    Ok(())
}

/// Extract `(OutputType, ErrorType)` from a `Result<T, E>` return type.
/// Falls back to `((), ())` if the return type is not `Result<_, _>`.
fn extract_result_types(
    output: &ReturnType,
) -> (proc_macro2::TokenStream, proc_macro2::TokenStream) {
    match output {
        ReturnType::Type(_, ty) => {
            if let Type::Path(type_path) = &**ty {
                if let Some(segment) = type_path.path.segments.last() {
                    if segment.ident == "Result" {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            let mut types = args.args.iter().filter_map(|a| {
                                if let syn::GenericArgument::Type(t) = a {
                                    Some(t)
                                } else {
                                    None
                                }
                            });
                            let output_type = types
                                .next()
                                .map(|t| quote!(#t))
                                .unwrap_or_else(|| quote!(()));
                            let error_type = types
                                .next()
                                .map(|t| quote!(#t))
                                .unwrap_or_else(|| quote!(()));
                            return (output_type, error_type);
                        }
                    }
                }
            }
            (quote!((())), quote!((())))
        }
        ReturnType::Default => (quote!((())), quote!((()))),
    }
}

// ── 入口 ─────────────────────────────────────────────────────────────────────

/// Generate a `Tool` implementation for a function.
///
/// This macro creates:
/// 1. A parameter struct with `#[derive(serde::Deserialize, schemars::JsonSchema)]`
/// 2. A zero-sized tool struct (PascalCase of the function name)
/// 3. `impl peco_core::tools::Tool for ToolStruct`
/// 4. A `static TOOL_NAME: ToolStruct = ToolStruct;` for convenience
///
/// The original function is preserved as-is (minus parameter doc attributes,
/// which are invalid on function arguments).
#[proc_macro_attribute]
pub fn peco_tool(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as MacroArgs);
    let input_fn = parse_macro_input!(input as syn::ItemFn);

    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();
    let tool_name = args.name.clone().unwrap_or_else(|| fn_name_str.clone());
    let vis = &input_fn.vis;

    // Validate tool name
    if let Err(e) = validate_tool_name(&tool_name) {
        return e.to_compile_error().into();
    }

    // ── Clean the function: remove doc attributes from parameters ─────────
    // `#[doc = "..."]` on function arguments is rejected by the compiler.
    let cleaned_fn = {
        let mut f = input_fn.clone();
        for arg in f.sig.inputs.iter_mut() {
            if let syn::FnArg::Typed(pat_type) = arg {
                pat_type.attrs.retain(|a| !a.path().is_ident("doc"));
            }
        }
        f
    };

    let is_async = input_fn.sig.asyncness.is_some();

    // ── Extract Result<T, E> ─────────────────────────────────────────────
    let (output_type, error_type) = extract_result_types(&input_fn.sig.output);

    // ── Build struct names ────────────────────────────────────────────────
    let struct_name = format_ident!("{}", fn_name_str.to_case(Case::Pascal));
    let params_struct_name = format_ident!("{}Parameters", struct_name);
    let static_name = format_ident!("{}", fn_name_str.to_uppercase());

    // ── Description resolution: explicit > doc comment > default ──────────
    let fn_doc = extract_doc_comment(&input_fn.attrs);
    let tool_description = match &args.description {
        Some(desc) => quote! { #desc.to_string() },
        None => match &fn_doc {
            Some(doc) => quote! { #doc.to_string() },
            None => quote! { format!("Call the {} tool", Self::NAME) },
        },
    };

    // ── Collect parameter names, types, and descriptions ──────────────────
    let mut param_names: Vec<Ident> = Vec::new();
    let mut field_tokens: Vec<proc_macro2::TokenStream> = Vec::new();

    for arg in input_fn.sig.inputs.iter() {
        if let syn::FnArg::Typed(pat_type) = arg {
            if let syn::Pat::Ident(param_ident) = &*pat_type.pat {
                let param_name = &param_ident.ident;
                let param_name_str = param_name.to_string();
                let ty = &pat_type.ty;

                // Field description: explicit > doc comment on param > default
                let field_doc = if let Some(explicit) = args.param_descriptions.get(&param_name_str)
                {
                    quote! { #[schemars(description = #explicit)] }
                } else if let Some(doc) = extract_doc_comment(&pat_type.attrs) {
                    quote! { #[schemars(description = #doc)] }
                } else {
                    let default_desc = format!("Parameter `{param_name_str}`");
                    quote! { #[schemars(description = #default_desc)] }
                };

                // Option<T> → #[serde(default)]
                let serde_default = if is_option_type(ty) {
                    quote! { #[serde(default)] }
                } else {
                    quote! {}
                };

                field_tokens.push(quote! {
                    #field_doc
                    #serde_default
                    #vis #param_name: #ty
                });
                param_names.push(param_name.clone());
            }
        }
    }

    // ── Required list: explicit > all params ──────────────────────────────
    let required_args: Vec<String> = args
        .required
        .unwrap_or_else(|| param_names.iter().map(|n| n.to_string()).collect());

    // ── call() implementation ─────────────────────────────────────────────
    let call_impl = if is_async {
        quote! {
            async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
                #fn_name(#(args.#param_names,)*).await
            }
        }
    } else {
        quote! {
            async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
                #fn_name(#(args.#param_names,)*)
            }
        }
    };

    let peco_core = peco_core_path();

    let expanded = quote! {
        #[derive(serde::Deserialize, schemars::JsonSchema)]
        #vis struct #params_struct_name {
            #(#field_tokens,)*
        }

        #cleaned_fn

        #[derive(Default)]
        #vis struct #struct_name;

        impl #peco_core::tools::Tool for #struct_name {
            const NAME: &'static str = #tool_name;

            type Args = #params_struct_name;
            type Output = #output_type;
            type Error = #error_type;

            fn name(&self) -> String {
                #tool_name.to_string()
            }

            fn definition(&self) -> #peco_core::tools::ToolDefinition {
                let mut schema = serde_json::to_value(
                    schemars::schema_for!(#params_struct_name)
                ).expect("schema serialization failed");
                schema["required"] = serde_json::json!([#(#required_args),*]);

                #peco_core::tools::ToolDefinition {
                    name: #tool_name.to_string(),
                    description: #tool_description,
                    parameters: schema,
                }
            }

            #call_impl
        }

        #[allow(dead_code)]
        #vis static #static_name: #struct_name = #struct_name;
    };

    TokenStream::from(expanded)
}
