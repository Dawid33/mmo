// TODO: Implement sanity check for debug builds to see if the log in Undo<T>'s
//are actually the correct pointer. Logging can easily break if Undo were to be
//build with default out side the macro and assigned to a field. Or find some
//way to make this impossible.
#![allow(unused)]
extern crate proc_macro;
use std::collections::{BTreeMap, HashSet};

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{ToTokens, TokenStreamExt, quote};
use syn::{
    Attribute, DataStruct, DeriveInput, FieldsNamed, Ident, Item, ItemStruct, Meta, Visibility,
    parse::{Parse, Parser},
    parse_macro_input, parse_quote,
    punctuated::Punctuated,
    spanned::Spanned,
    token::Mod,
};

fn boilerplate(items: &mut Vec<Item>, root_struct_ident: &Ident) {
    items.push(item(
        "Undo AsRef",
        quote! {
            impl<T: ::core::default::Default> AsRef<T> for Undo<T> {
                fn as_ref(&self) -> &T {
                    &self.data
                }
            }
        },
    ));

    items.push(item(
        "Undo Deref",
        quote! {
            impl<T> ::std::ops::Deref for Undo<T>
            where T: Default {
                type Target = T;

                fn deref(&self) -> &Self::Target {
                    &self.data
                }
            }
        },
    ));

    items.push(item(
        "Undo DerefMut",
        quote! {
            impl<T> ::std::ops::DerefMut for Undo<T>
            where T: Default {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut self.data
                }
            }
        },
    ));

    items.push(item(
        "Rollback Deref",
        quote! {
            impl ::std::ops::Deref for Rollback {
                type Target = #root_struct_ident;

                fn deref(&self) -> &Self::Target {
                    &self.data
                }
            }
        },
    ));

    items.push(item(
        "Rollback DerefMut",
        quote! {
            impl ::std::ops::DerefMut for Rollback {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut self.data
                }
            }
        },
    ));
}

fn item(name: &str, s: proc_macro2::TokenStream) -> syn::Item {
    syn::Item::parse
        .parse2(s.into())
        .expect(&format!("{:?} failed to parse.", name))
}

#[proc_macro_attribute]
pub fn rollback(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut ast = parse_macro_input!(input as syn::ItemMod);
    let mut args = syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated
        .parse(args)
        .unwrap();

    if args.len() > 1 {
        panic!("Cannot have more than one argument to rollback macro.");
    } else if args.len() < 1 {
        panic!("Must supply name of struct that will have rollback functions.");
    }

    let module_ident = ast.ident.clone();
    let root_struct_ident = args.first().unwrap().require_ident().unwrap();
    let root_struct_ident_string = root_struct_ident.to_string();

    let content = if let Some(c) = &mut ast.content {
        if c.1.len() == 0 {
            return quote! { #ast }.into();
        }
        c
    } else {
        // Empty module. Do nothing.
        return quote! { #ast }.into();
    };

    let items: &mut Vec<Item> = content.1.as_mut();
    let find = |needle: &String| -> Option<&ItemStruct> {
        let s = items.iter().find(|s| match s {
            Item::Struct(item_struct) => {
                if &item_struct.ident.to_string() == needle {
                    true
                } else {
                    false
                }
            }
            _ => false,
        });

        if let Some(s) = s {
            match s {
                Item::Struct(item_struct) => Some(item_struct),
                _ => panic!("Cannot happen."),
            }
        } else {
            None
        }
    };

    // Build path from root to each field.
    let mut path_stack: Vec<&Ident> = Vec::new();
    let mut struct_stack: Vec<(&ItemStruct, usize)> =
        Vec::from([(find(&root_struct_ident_string).unwrap(), 0)]);
    let mut paths: Vec<(proc_macro2::TokenStream, syn::Field)> = Vec::new();
    while let Some((s, current)) = struct_stack.pop() {
        match &s.fields {
            syn::Fields::Named(fields_named) => {
                if let Some(f) = &fields_named.named.get(current) {
                    struct_stack.push((s, current + 1));
                    let ident = f.ident.as_ref().unwrap();
                    let stack = path_stack.iter();
                    paths.push((quote! { #(#stack.)*#ident }, f.to_owned().clone()));
                    if let Some(s) = find(&f.ty.to_token_stream().to_string()) {
                        path_stack.push(ident);
                        struct_stack.push((s, 0));
                    }
                } else {
                    if !path_stack.is_empty() {
                        path_stack.pop();
                    }
                    continue;
                }
            }
            syn::Fields::Unnamed(fields_unnamed) => panic!("Cannot have unnamed fields."),
            syn::Fields::Unit => panic!("Cannot have unit fields."),
        }
    }

    let mut all_fields = Vec::new();
    for mut i in items.iter_mut() {
        match &mut i {
            Item::Struct(item_struct) => {
                item_struct.attrs.push(
                    parse_quote! {#[derive(::core::default::Default, ::rollback::Debug, ::rollback::serde::Serialize, ::rollback::serde::Deserialize, ::core::clone::Clone, ::borrow::Partial)] },
                );
                item_struct.attrs.push(
                    parse_quote! {#[module(crate)]},
                );

                match &mut item_struct.fields {
                    syn::Fields::Named(named_fields) => {
                        for f in &mut named_fields.named {
                            all_fields.push(f.clone());
                            let ty = &f.ty;
                            f.ty = syn::Type::parse
                                .parse2(quote! { Undo<#ty> })
                                .expect("Failed to change type of field to Undo<T>");
                            f.vis = parse_quote! {pub};
                        }
                    }
                    _ => (),
                }
            }
            _ => (),
        }
    }

    let iter_ident = paths
        .iter()
        .map(|f| f.1.ident.clone().unwrap())
        .collect::<Vec<syn::Ident>>()
        .into_iter();
    let iter_log_ident = paths
        .iter()
        .enumerate()
        .map(|(i, f)| {
            Ident::new(
                &format!("{}{}", f.1.ident.as_ref().unwrap(), i),
                Span::call_site(),
            )
        })
        .collect::<Vec<syn::Ident>>()
        .into_iter();
    let iter_log_ident_index = paths
        .iter()
        .enumerate()
        .map(|(i, f)| i)
        .collect::<Vec<usize>>()
        .into_iter();
    let iter_ty = paths
        .iter()
        .map(|f| f.1.ty.clone())
        .collect::<Vec<_>>()
        .into_iter();
    let iter_path = paths
        .iter()
        .map(|f| f.0.clone())
        .collect::<Vec<proc_macro2::TokenStream>>()
        .into_iter();

    boilerplate(items, root_struct_ident);
    items.push(item(
        "struct Undo<T>",
        quote! {
            #[derive(::core::default::Default, ::rollback::Debug, ::serde::Serialize, ::serde::Deserialize, ::core::clone::Clone)]
            pub struct Undo<T> where T: Default + 'static /*+ ::serde::Serialize + for<'a> ::serde::Deserialize<'a>*/ {
                #[serde(skip)]
                #[debug(skip)]
                log: ::std::sync::Arc<::std::sync::Mutex<::std::collections::VecDeque<Box<dyn Fn(&mut T) + Send>>>>,
                #[serde(skip)]
                #[debug(skip)]
                global_log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
                #[serde(skip)]
                #[debug(skip)]
                info: ::rollback::RollbackInfo,
                #[serde(skip)]
                #[debug(skip)]
                field: usize,
                #[debug(skip)]
                data: T
            }
        },
    ));

    items.push(item(
        "impl Undo<T>",
        quote! {
            impl<T> Undo<T> where T: Default + 'static {
                pub fn undo(&mut self, undo: impl Fn(&mut T) + 'static + Send) {
                    let mut global = self.global_log.lock().unwrap();
                    let mut local = self.log.lock().unwrap();
                    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    local.push_back(Box::new(undo));
                    global.log.push_back((trans, self.field));
                }
            }
        },
    ));

    let iter_ty1 = iter_ty.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    items.push(item(
        "RollbackLog",
        quote! {
            #[derive(::core::default::Default)]
            pub struct RollbackLog {
                pub log: ::std::collections::VecDeque<(usize, usize)>,
                info: ::rollback::RollbackInfo,
                #(#iter_log_ident1 : ::std::sync::Arc<::std::sync::Mutex<::std::collections::VecDeque<Box<dyn Fn(&mut #iter_ty1) + Send>>>>,)*
            }
        },
    ));

    items.push(item(
        "Rollback",
        quote! {
            #[derive(::serde::Serialize, ::serde::Deserialize, ::core::clone::Clone)]
            pub struct Rollback {
                #[serde(skip)]
                pub log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
                data: Undo<#root_struct_ident>,
            }
        },
    ));

    let iter_log_ident_index1 = iter_log_ident_index.clone();
    let iter_log_ident_index2 = iter_log_ident_index.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    let iter_log_ident2 = iter_log_ident.clone();
    let iter_path1 = iter_path.clone();
    let iter_log_ident_index2 = iter_log_ident_index.clone();
    let iter_log_ident2 = iter_log_ident.clone();
    let iter_path2 = iter_path.clone();
    let iter_path3 = iter_path.clone();
    items.push(item(
        "impl Rollback",
        quote! {
            impl Rollback {
                pub fn current(&self) -> usize {
                    self.log.lock().unwrap().info.current.load(::std::sync::atomic::Ordering::SeqCst)
                }
                pub fn oldest(&self) -> usize {
                    self.log.lock().unwrap().info.oldest.load(::std::sync::atomic::Ordering::SeqCst)
                }
                pub fn new_transaction(&mut self) {
                    self.log.lock().unwrap().info.current.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
                }
                pub fn rollback(&mut self) {
                    let rollback_log = self.log.clone();
                    let mut rollback_log = rollback_log.lock().unwrap();
                    let oldest = rollback_log.info.oldest.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    let mut actually_rolled_something_back = false;
                    while let Some((transaction, field)) = rollback_log.log.pop_back().clone() {
                        if current == transaction {
                            actually_rolled_something_back = true;
                            match field {
                                #(#iter_log_ident_index1 => {
                                    let func = rollback_log.#iter_log_ident1.lock().unwrap().pop_back().unwrap();
                                    func(&mut self.#iter_path1.data);
                                })*
                                _ => panic!("Tried to undo field that doesn't exist.")
                            }
                        } else {
                            rollback_log.log.push_back((transaction, field));
                            break;
                        }
                    }
                    if oldest >= current {
                        rollback_log.info.oldest.store(oldest - 1, ::std::sync::atomic::Ordering::SeqCst).clone();
                    }
                    rollback_log.info.current.store(current - 1, ::std::sync::atomic::Ordering::SeqCst).clone();
                }
                pub fn forget(&mut self) {
                    let rollback_log = self.log.clone();
                    let mut rollback_log = rollback_log.lock().unwrap();
                    let oldest = rollback_log.info.oldest.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    if oldest >= current {
                        return;
                    }
                    while let Some((transaction, field)) = rollback_log.log.pop_front().clone() {
                        if oldest != transaction {
                            rollback_log.log.push_front((transaction, field));
                            break;
                        } 
                        match field {
                            #(#iter_log_ident_index2 => {
                                rollback_log.#iter_log_ident2.lock().unwrap().pop_back().unwrap();
                            })*
                            _ => panic!("Tried to forget field that doesn't exist.")
                        }
                    }
                    rollback_log.info.oldest.store(oldest + 1, ::std::sync::atomic::Ordering::SeqCst).clone();
                }
            }
        },
    ));

    let iter_path1 = iter_path.clone();
    let iter_path2 = iter_path.clone();
    let iter_path3 = iter_path.clone();
    let iter_path4 = iter_path.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    let iter_log_ident_index1 = iter_log_ident_index.clone();
    items.push(item(
        "impl Default for Rollback",
        quote! {
            impl ::core::default::Default for Rollback {
                fn default() -> Self {
                    use ::std::ops::DerefMut;
                    let log = ::std::sync::Arc::new(::std::sync::Mutex::new(RollbackLog::default()));
                    let mut r = Self {
                        log: log.clone() ,
                        data: Undo::default(),
                    };
                    #(r.#iter_path2.global_log = log.clone();)*
                    let mut log = log.lock().unwrap();
                    #(r.#iter_path1.log = log.#iter_log_ident1.clone();)*
                    #(r.#iter_path3.info = log.info.clone();)*
                    #(r.#iter_path4.field = #iter_log_ident_index1;)*
                    drop(log);
                    r
                }
            }
        },
    ));

    let iter_path1 = iter_path.clone();
    let iter_path2 = iter_path.clone();
    let iter_path3 = iter_path.clone();
    let iter_path4 = iter_path.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    let iter_log_ident_index1 = iter_log_ident_index.clone();
    items.push(item(
        "impl Rollback reinitialize",
        quote! {
            impl Rollback {
                pub fn reinitialize(&mut self) {
                    use ::std::ops::DerefMut;
                    let log = ::std::sync::Arc::new(::std::sync::Mutex::new(RollbackLog::default()));
                    self.log = log.clone();
                    #(self.#iter_path2.global_log = log.clone();)*
                    let mut log = log.lock().unwrap();
                    #(self.#iter_path1.log = log.#iter_log_ident1.clone();)*
                    #(self.#iter_path3.info = log.info.clone();)*
                    #(self.#iter_path4.field = #iter_log_ident_index1;)*
                }
            }
        },
    ));

    quote! { #ast }.into()
}
