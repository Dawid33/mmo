//! `#[derive(StateHash)]`: fields in declaration order (like `#[derive(Hash)]`);
//! enums write the variant's index as `u32` first; `#[state_hash(skip)]` on a
//! field leaves it out (caches, workspaces); `#[state_hash(with = "path")]`
//! hashes it as `path(&field, hasher)` (e.g. an unordered map, sorted). Every
//! type parameter gets a `StateHash` bound; `#[state_hash(bound = "…")]` on the type replaces them.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, parse_macro_input, parse_quote};

#[proc_macro_derive(StateHash, attributes(state_hash))]
pub fn derive_state_hash(input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as DeriveInput);
    match expand(&mut input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

enum Attr {
    Skip,
    Bound(Box<syn::WherePredicate>),
    With(syn::Path),
}

fn attrs(attrs: &[syn::Attribute]) -> syn::Result<Vec<Attr>> {
    let mut out = Vec::new();
    for a in attrs.iter().filter(|a| a.path().is_ident("state_hash")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("skip") {
                out.push(Attr::Skip);
                Ok(())
            } else if m.path.is_ident("bound") {
                let s: syn::LitStr = m.value()?.parse()?;
                let preds = s.parse_with(
                    syn::punctuated::Punctuated::<syn::WherePredicate, syn::Token![,]>::parse_terminated,
                )?;
                out.extend(preds.into_iter().map(|p| Attr::Bound(Box::new(p))));
                Ok(())
            } else if m.path.is_ident("with") {
                let s: syn::LitStr = m.value()?.parse()?;
                out.push(Attr::With(s.parse()?));
                Ok(())
            } else {
                Err(m.error("expected `skip`, `with = \"path\"` or `bound = \"...\"`"))
            }
        })?;
    }
    Ok(out)
}

/// Statements hashing `fields`, each bound to the matching name in `names`:
/// skipped, through a `with` function, or through `StateHash`.
fn hash_fields(fields: &Fields, names: &[TokenStream2]) -> syn::Result<TokenStream2> {
    let mut out = TokenStream2::new();
    for (f, n) in fields.iter().zip(names) {
        let field_attrs = attrs(&f.attrs)?;
        if field_attrs.iter().any(|a| matches!(a, Attr::Skip)) {
            continue;
        }
        match field_attrs.iter().find_map(|a| match a {
            Attr::With(p) => Some(p),
            _ => None,
        }) {
            Some(path) => out.extend(quote! { #path(#n, __h); }),
            None => out.extend(quote! { ::state_hash::StateHash::state_hash(#n, __h); }),
        }
    }
    Ok(out)
}

fn expand(input: &mut DeriveInput) -> syn::Result<TokenStream2> {
    let type_attrs = attrs(&input.attrs)?;
    let custom_bounds: Vec<_> = type_attrs
        .into_iter()
        .filter_map(|a| match a {
            Attr::Bound(p) => Some(*p),
            _ => None,
        })
        .collect();
    let where_clause = input.generics.make_where_clause();
    if custom_bounds.is_empty() {
        let params: Vec<_> = input
            .generics
            .type_params()
            .map(|p| p.ident.clone())
            .collect();
        let wc = input.generics.make_where_clause();
        for p in params {
            wc.predicates
                .push(parse_quote!(#p: ::state_hash::StateHash));
        }
    } else {
        where_clause.predicates.extend(custom_bounds);
    }

    let body = match &input.data {
        Data::Struct(s) => {
            let names: Vec<TokenStream2> = match &s.fields {
                Fields::Named(n) => n
                    .named
                    .iter()
                    .map(|f| {
                        let i = f.ident.as_ref().unwrap();
                        quote!(&self.#i)
                    })
                    .collect(),
                Fields::Unnamed(u) => (0..u.unnamed.len())
                    .map(|i| {
                        let i = syn::Index::from(i);
                        quote!(&self.#i)
                    })
                    .collect(),
                Fields::Unit => Vec::new(),
            };
            hash_fields(&s.fields, &names)?
        }
        Data::Enum(e) => {
            let mut arms = TokenStream2::new();
            for (idx, v) in e.variants.iter().enumerate() {
                let vi = &v.ident;
                let idx = idx as u32;
                let binds: Vec<_> = (0..v.fields.len())
                    .map(|i| format_ident!("__f{}", i))
                    .collect();
                let names: Vec<TokenStream2> = binds.iter().map(|b| quote!(#b)).collect();
                let stmts = hash_fields(&v.fields, &names)?;
                let pat = match &v.fields {
                    Fields::Named(n) => {
                        let idents = n.named.iter().map(|f| f.ident.as_ref().unwrap());
                        quote!(Self::#vi { #(#idents: #binds),* })
                    }
                    Fields::Unnamed(_) => quote!(Self::#vi(#(#binds),*)),
                    Fields::Unit => quote!(Self::#vi),
                };
                arms.extend(quote! {
                    #[allow(unused_variables)]
                    #pat => {
                        ::core::hash::Hasher::write_u32(__h, #idx);
                        #stmts
                    }
                });
            }
            if e.variants.is_empty() {
                quote!(match *self {})
            } else {
                quote!(match self { #arms })
            }
        }
        Data::Union(u) => {
            return Err(syn::Error::new_spanned(
                u.union_token,
                "StateHash cannot be derived for unions",
            ));
        }
    };

    let name = &input.ident;
    let (impl_g, ty_g, where_g) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_g ::state_hash::StateHash for #name #ty_g #where_g {
            #[allow(unused_variables)]
            fn state_hash<__H: ::core::hash::Hasher>(&self, __h: &mut __H) {
                #body
            }
        }
    })
}
