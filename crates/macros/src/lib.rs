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
            impl<T> AsRef<T> for Undo<T>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
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
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
                type Target = T;

                fn deref(&self) -> &Self::Target {
                    &self.data
                }
            }
        },
    ));

    items.push(item(
        "Undo DerefMut Clone",
        quote! {
            impl<T> ::std::ops::DerefMut for Undo<T>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut self.data
                }
            }
        },
    ));

    items.push(item(
        "mut access",
        quote! {
            impl<T> Undo<T>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
                pub fn change(&mut self) -> &mut T {
                    let old = self.data.clone();
                    self.undo(move |mut d, s| *d = old);
                    &mut self.data
                }
            }
        },
    ));

    items.push(item(
        "DelayedUndo Deref",
        quote! {
            impl<T, 'a> ::std::ops::Deref for DelayedUndo<T, 'a>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
                type Target = T;

                fn deref(&self) -> &Self::Target {
                    &self.value
                }
            }
        },
    ));

    items.push(item(
        "DelayedUndo DerefMut",
        quote! {
            impl<T, 'a> ::std::ops::DerefMut for DelayedUndo<T, 'a>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
                fn deref_mut(&mut self) -> &mut Self::Target {
                    &mut self.value
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

/// Reads the `#[undo(kind)]` attribute off a field, if present.
fn undo_kind(f: &syn::Field) -> Option<String> {
    f.attrs.iter().find_map(|a| {
        if !a.path().is_ident("undo") {
            return None;
        }
        a.parse_args::<syn::Ident>().ok().map(|i| i.to_string())
    })
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
    let mut paths: Vec<(proc_macro2::TokenStream, syn::Field, Option<String>)> = Vec::new();
    while let Some((s, current)) = struct_stack.pop() {
        match &s.fields {
            syn::Fields::Named(fields_named) => {
                if let Some(f) = &fields_named.named.get(current) {
                    struct_stack.push((s, current + 1));
                    let ident = f.ident.as_ref().unwrap();
                    let stack = path_stack.iter();
                    paths.push((quote! { #(#stack.)*#ident }, f.to_owned().clone(), undo_kind(f)));
                    let stack = path_stack.iter();
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
    for p in &paths {
        if let Some(kind) = &p.2 {
            if kind != "cell" {
                panic!("unknown undo kind `{}` on field `{}`", kind, p.0);
            }
        }
    }

    let mut all_fields = Vec::new();
    for mut i in items.iter_mut() {
        match &mut i {
            Item::Struct(item_struct) => {
                item_struct.attrs.push(
                    parse_quote! {#[derive(::core::default::Default, ::derive_more::Debug, crate::serde::Serialize, crate::serde::Deserialize, ::std::clone::Clone, ::borrow::Partial, ::std::hash::Hash)] },
                );
                item_struct.attrs.push(parse_quote! {#[module(crate)]});

                match &mut item_struct.fields {
                    syn::Fields::Named(named_fields) => {
                        for f in &mut named_fields.named {
                            all_fields.push(f.clone());
                            let kind = undo_kind(f);
                            f.attrs.retain(|a| !a.path().is_ident("undo"));
                            let ty = &f.ty;
                            f.ty = match kind.as_deref() {
                                Some("cell") => syn::Type::parse
                                    .parse2(quote! { UndoCell<#ty> })
                                    .expect("Failed to change type of field to UndoCell<T>"),
                                Some(other) => panic!("unknown undo kind `{}`", other),
                                None => syn::Type::parse
                                    .parse2(quote! { Undo<#ty> })
                                    .expect("Failed to change type of field to Undo<T>"),
                            };
                            f.vis = parse_quote! {pub};
                        }
                    }
                    _ => (),
                }
            }
            _ => (),
        }
    }

    // Every field path, regardless of wrapper kind: used to wire global_log/info.
    let all_path = paths
        .iter()
        .map(|f| f.0.clone())
        .collect::<Vec<proc_macro2::TokenStream>>();

    // Tier-2 (opaque closure) fields: everything without an #[undo(...)] kind.
    let opaque: Vec<(usize, &(proc_macro2::TokenStream, syn::Field, Option<String>))> = paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.2.is_none())
        .collect();
    // Tier-1 cell fields.
    let cells: Vec<(usize, &(proc_macro2::TokenStream, syn::Field, Option<String>))> = paths
        .iter()
        .enumerate()
        .filter(|(_, p)| p.2.as_deref() == Some("cell"))
        .collect();

    let log_ident_of = |i: usize, f: &syn::Field| {
        Ident::new(
            &format!("{}{}", f.ident.as_ref().unwrap(), i),
            Span::call_site(),
        )
    };
    let iter_log_ident = opaque
        .iter()
        .map(|(i, p)| log_ident_of(*i, &p.1))
        .collect::<Vec<syn::Ident>>()
        .into_iter();
    let iter_ty = opaque
        .iter()
        .map(|(_, p)| p.1.ty.clone())
        .collect::<Vec<_>>()
        .into_iter();
    let iter_path = opaque
        .iter()
        .map(|(_, p)| p.0.clone())
        .collect::<Vec<proc_macro2::TokenStream>>()
        .into_iter();
    let iter_path_string = opaque
        .iter()
        .map(|(_, p)| p.0.clone().to_token_stream().to_string())
        .collect::<Vec<String>>()
        .into_iter();

    let cell_log_ident = cells
        .iter()
        .map(|(i, p)| log_ident_of(*i, &p.1))
        .collect::<Vec<syn::Ident>>()
        .into_iter();
    let cell_ty = cells
        .iter()
        .map(|(_, p)| p.1.ty.clone())
        .collect::<Vec<_>>()
        .into_iter();
    let cell_path = cells
        .iter()
        .map(|(_, p)| p.0.clone())
        .collect::<Vec<proc_macro2::TokenStream>>()
        .into_iter();
    let cell_path_string = cells
        .iter()
        .map(|(_, p)| p.0.clone().to_token_stream().to_string())
        .collect::<Vec<String>>()
        .into_iter();

    boilerplate(items, root_struct_ident);
    items.push(item(
        "struct Delayed Undo<T>",
        quote! {
            pub struct DelayedUndo<T, 'a> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash {
                hash: u32,
                value: &'a mut Undo<T>
            }
        },
    ));

    items.push(item(
        "impl DelayedUndo<T>",
        quote! {
            impl<T, 'a> DelayedUndo<T, 'a> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + ::std::hash::Hash  {
                pub fn undo(&mut self, undo: impl FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + 'static + Send) {
                    let mut global = self.value.global_log.lock().unwrap();
                    let trans = self.value.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    let wrap = self.value.wrap.expect("Undo field not wired to a FieldUndo variant");
                    global.log.push_back(Entry { transaction: trans, undo: UndoOp::Opaque(wrap(Box::new(undo))), pre_hash: self.hash });
                }
            }
        },
    ));

    items.push(item(
        "struct Undo<T>",
        quote! {
            #[derive(::core::default::Default, ::derive_more::Debug, ::serde::Serialize, ::serde::Deserialize, ::std::clone::Clone)]
            pub struct Undo<T> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static {
                #[serde(skip)]
                #[debug(skip)]
                global_log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
                #[serde(skip)]
                #[debug(skip)]
                info: RollbackInfo,
                #[serde(skip)]
                #[debug(skip)]
                wrap: ::std::option::Option<fn(Box<dyn FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>) -> FieldUndo>,
                #[debug(skip)]
                data: T
            }
        },
    ));

    items.push(item(
        "impl hash Undo<T>",
        quote! {
            impl<T> ::std::hash::Hash for Undo<T>
                where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static + ::std::hash::Hash
            {
                fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {
                    self.data.hash(state);
                }
            }
        },
    ));

    items.push(item(
        "impl Undo<T>",
        quote! {
            impl<T> Undo<T> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + 'static + Sized + ::std::hash::Hash {
                pub fn undo(&mut self, undo: impl FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + 'static + Send) {
                    let mut global = self.global_log.lock().unwrap();
                    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    let pre_hash = unsafe { self.hash_data() };
                    let wrap = self.wrap.expect("Undo field not wired to a FieldUndo variant");
                    global.log.push_back(Entry { transaction: trans, undo: UndoOp::Opaque(wrap(Box::new(undo))), pre_hash });
                }

                pub fn delayed_undo(&mut self) -> DelayedUndo<T, '_> {
                    DelayedUndo {
                        hash: unsafe { self.hash_data() },
                        value: self
                    }
                }

                pub unsafe fn hash_data(&self) -> u32 {
                    let mut hasher = ::crc32fast::Hasher::new();
                    self.data.hash(&mut hasher);
                    hasher.finalize()
                }

                pub fn print_log(&mut self) {
                    let global = self.global_log.lock().unwrap();
                    let entries: Vec<(usize, u32)> = global.log.iter().map(|e| (e.transaction, e.pre_hash)).collect();
                    ::log::info!("{:?}", entries);
                }

                pub fn send(&self, value: crate::GameDataUpdate) {
                    let mut global = self.global_log.lock().unwrap();
                    global.client.as_ref().inspect(move |s| {
                        s.send(value).unwrap();
                    });
                }
            }
        },
    ));

    let iter_ty1 = iter_ty.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    items.push(item(
        "FieldUndo",
        quote! {
            // One variant per field path, plus the root struct. Tuple-variant
            // constructors double as the `wrap` fn pointers stored on Undo<T>.
            #[allow(non_camel_case_types)]
            pub enum FieldUndo {
                root(Box<dyn FnOnce(&mut #root_struct_ident, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>),
                #(#iter_log_ident1(Box<dyn FnOnce(&mut #iter_ty1, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>),)*
            }
        },
    ));
    items.push(item(
        "Entry",
        quote! {
            pub struct Entry {
                pub transaction: usize,
                pub undo: UndoOp,
                pub pre_hash: u32,
            }
        },
    ));
    items.push(item(
        "UndoOp",
        quote! {
            pub enum UndoOp {
                Opaque(FieldUndo),
                Typed(Delta),
            }
        },
    ));
    let cell_log_ident1 = cell_log_ident.clone();
    let cell_ty1 = cell_ty.clone();
    items.push(item(
        "Delta",
        quote! {
            // Typed, inspectable undo records for tier-1 fields. One tuple
            // variant per field holding the OLD value; the constructor doubles
            // as the `make` fn pointer stored on the wrapper.
            #[allow(non_camel_case_types)]
            pub enum Delta {
                #(#cell_log_ident1(#cell_ty1),)*
            }
        },
    ));
    items.push(item(
        "UndoCell",
        quote! {
            #[derive(::core::default::Default, ::derive_more::Debug, ::serde::Serialize, ::serde::Deserialize, ::std::clone::Clone)]
            pub struct UndoCell<T> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static {
                #[serde(skip)]
                #[debug(skip)]
                global_log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
                #[serde(skip)]
                #[debug(skip)]
                info: RollbackInfo,
                #[serde(skip)]
                #[debug(skip)]
                make: ::std::option::Option<fn(T) -> Delta>,
                #[debug(skip)]
                data: T
            }
        },
    ));
    items.push(item(
        "impl UndoCell",
        quote! {
            impl<T> UndoCell<T> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static {
                fn log_old(&mut self) {
                    let mut global = self.global_log.lock().unwrap();
                    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    let mut hasher = ::crc32fast::Hasher::new();
                    ::std::hash::Hash::hash(&self.data, &mut hasher);
                    let pre_hash = hasher.finalize();
                    let make = self.make.expect("UndoCell not wired to a Delta variant");
                    global.log.push_back(Entry { transaction: trans, undo: UndoOp::Typed(make(self.data.clone())), pre_hash });
                }
                pub fn set(&mut self, v: T) {
                    self.log_old();
                    self.data = v;
                }
                pub fn update(&mut self, f: impl FnOnce(&mut T)) {
                    self.log_old();
                    f(&mut self.data);
                }
                pub unsafe fn hash_data(&self) -> u32 {
                    let mut hasher = ::crc32fast::Hasher::new();
                    ::std::hash::Hash::hash(&self.data, &mut hasher);
                    hasher.finalize()
                }
            }
        },
    ));
    items.push(item(
        "UndoCell Deref",
        quote! {
            impl<T> ::std::ops::Deref for UndoCell<T>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static {
                type Target = T;

                fn deref(&self) -> &Self::Target {
                    &self.data
                }
            }
        },
    ));
    items.push(item(
        "UndoCell Hash",
        quote! {
            impl<T> ::std::hash::Hash for UndoCell<T>
            where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static {
                fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {
                    self.data.hash(state);
                }
            }
        },
    ));
    items.push(item(
        "RollbackLog",
        quote! {
            #[derive(::core::default::Default)]
            pub struct RollbackLog {
                pub log: ::std::collections::VecDeque<Entry>,
                pub client: ::std::option::Option<::crossbeam::channel::Sender<crate::GameDataUpdate>>,
                info: RollbackInfo,
            }
        },
    ));

    items.push(item(
        "Rollback",
        quote! {
            #[derive(::serde::Serialize, ::serde::Deserialize, ::std::clone::Clone, ::borrow::Partial, ::derive_more::Debug)]
            #[module(crate)]
            pub struct Rollback {
                #[serde(skip)]
                #[debug(skip)]
                pub log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
                #[debug(skip)]
                pub data: Undo<#root_struct_ident>,
            }
        },
    ));

    items.push(item(
        "impl hash Rollback",
        quote! {
            impl ::std::hash::Hash for Rollback {
                fn hash<H: ::std::hash::Hasher>(&self, state: &mut H) {
                    self.data.hash(state);
                }
            }
        },
    ));

    let iter_log_ident1 = iter_log_ident.clone();
    let iter_log_ident2 = iter_log_ident.clone();
    let iter_path1 = iter_path.clone();
    let iter_log_ident2 = iter_log_ident.clone();
    let iter_path2 = iter_path.clone();
    let iter_path3 = iter_path.clone();
    let iter_path4 = iter_path.clone();
    let iter_path5 = iter_path.clone();
    let iter_path6 = iter_path.clone();
    let iter_path7 = iter_path.clone();
    let iter_path_string1 = iter_path_string.clone();
    let cell_log_ident2 = cell_log_ident.clone();
    let cell_path1 = cell_path.clone();
    let cell_path_string1 = cell_path_string.clone();
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
                    use ::std::hash::{Hash, Hasher};
                    let rollback_log = self.log.clone();
                    let mut rollback_log = rollback_log.lock().unwrap();
                    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    while let Some(entry) = rollback_log.log.pop_back() {
                        if entry.transaction != current {
                            rollback_log.log.push_back(entry);
                            break;
                        }
                        match entry.undo {
                            UndoOp::Opaque(FieldUndo::root(func)) => {
                                func(&mut self.data.data, rollback_log.client.as_ref().unwrap());
                                let new_hash = unsafe { self.data.hash_data() };
                                if new_hash != entry.pre_hash {
                                    panic!("Hash verification failed for root in transaction {:?}: {:?} != {:?}", entry.transaction, new_hash, entry.pre_hash);
                                }
                            }
                            #(UndoOp::Opaque(FieldUndo::#iter_log_ident1(func)) => {
                                let previous_data = self.#iter_path5.data.clone();
                                func(&mut self.#iter_path1.data, rollback_log.client.as_ref().unwrap());
                                let new_hash = unsafe { self.#iter_path6.hash_data() };
                                if new_hash != entry.pre_hash {
                                    println!("Hash verification failed for self.{}.hash_data() in transaction {:?} with new_hash != pre_hash: {:?} != {:?}\nremaining log len: {:?}", #iter_path_string1, entry.transaction, new_hash, entry.pre_hash, rollback_log.log.len());
                                    match ::assert_json_diff::assert_json_matches_no_panic(&self.#iter_path7.data, &previous_data, ::assert_json_diff::Config::new(::assert_json_diff::CompareMode::Strict)) {
                                        Ok(()) => panic!("Before and after is equal via serde_json"),
                                        Err(e) => panic!("lhs: new, rhs: old. {}", e),
                                    }
                                }
                            })*
                            UndoOp::Typed(delta) => match delta {
                                #(Delta::#cell_log_ident2(old) => {
                                    self.#cell_path1.data = old;
                                    let mut hasher = ::crc32fast::Hasher::new();
                                    ::std::hash::Hash::hash(&self.#cell_path1.data, &mut hasher);
                                    let new_hash = hasher.finalize();
                                    if new_hash != entry.pre_hash {
                                        panic!("Hash verification failed for typed undo of self.{} in transaction {:?}: {:?} != {:?}", #cell_path_string1, entry.transaction, new_hash, entry.pre_hash);
                                    }
                                })*
                            }
                        }
                    }
                    rollback_log.info.current.store(current - 1, ::std::sync::atomic::Ordering::SeqCst);
                }
                pub fn forget(&mut self) {
                    let rollback_log = self.log.clone();
                    let mut rollback_log = rollback_log.lock().unwrap();
                    let oldest = rollback_log.info.oldest.load(::std::sync::atomic::Ordering::SeqCst);
                    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    if oldest >= current {
                        panic!("Cannot forget transaction or transaction that doesn't exist. oldest, current = {:?}, {:?}", oldest, current);
                    }
                    while let Some(entry) = rollback_log.log.pop_front() {
                        if oldest + 1 < entry.transaction {
                            rollback_log.log.push_front(entry);
                            break;
                        }
                        // Entries are data; dropping them is the whole job.
                    }
                    rollback_log.info.oldest.store(oldest + 1, ::std::sync::atomic::Ordering::SeqCst);
                }
            }
        },
    ));

    let all_path1 = all_path.clone();
    let all_path2 = all_path.clone();
    let iter_path4 = iter_path.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    let cell_path2 = cell_path.clone();
    let cell_log_ident3 = cell_log_ident.clone();
    items.push(item(
        "impl new for Rollback",
        quote! {
            impl Rollback {
                pub fn new(client: ::std::option::Option<::crossbeam::channel::Sender<crate::GameDataUpdate>>) -> Self {
                    let mut log = RollbackLog::default();
                    log.client = client;
                    let log = ::std::sync::Arc::new(::std::sync::Mutex::new(log));
                    let mut r = Self {
                        log: log.clone() ,
                        data: Undo::default(),
                    };
                    r.data.global_log = log.clone();
                    r.data.wrap = Some(FieldUndo::root);
                    #(r.#all_path1.global_log = log.clone();)*
                    #(r.#iter_path4.wrap = Some(FieldUndo::#iter_log_ident1);)*
                    #(r.#cell_path2.make = Some(Delta::#cell_log_ident3);)*
                    let log = log.lock().unwrap();
                    r.data.info = log.info.clone();
                    #(r.#all_path2.info = log.info.clone();)*
                    drop(log);
                    r
                }
            }
        },
    ));

    let all_path1 = all_path.clone();
    let all_path2 = all_path.clone();
    let iter_path4 = iter_path.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    let cell_path2 = cell_path.clone();
    let cell_log_ident3 = cell_log_ident.clone();
    items.push(item(
        "impl Rollback reinitialize",
        quote! {
            impl Rollback {
                pub fn reinitialize(&mut self, client: ::std::option::Option<::crossbeam::channel::Sender<crate::GameDataUpdate>>) {
                    let mut log = RollbackLog::default();
                    log.client = client;
                    let log = ::std::sync::Arc::new(::std::sync::Mutex::new(log));
                    self.log = log.clone();
                    self.data.global_log = log.clone();
                    self.data.wrap = Some(FieldUndo::root);
                    #(self.#all_path1.global_log = log.clone();)*
                    #(self.#iter_path4.wrap = Some(FieldUndo::#iter_log_ident1);)*
                    #(self.#cell_path2.make = Some(Delta::#cell_log_ident3);)*
                    let log = log.lock().unwrap();
                    self.data.info = log.info.clone();
                    #(self.#all_path2.info = log.info.clone();)*
                }
            }
        },
    ));

    quote! { #ast }.into()
}
