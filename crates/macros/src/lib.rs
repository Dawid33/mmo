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

    let mut all_fields = Vec::new();
    for mut i in items.iter_mut() {
        match &mut i {
            Item::Struct(item_struct) => {
                item_struct.attrs.push(
                    parse_quote! {#[derive(::core::default::Default, ::rollback::Debug, ::rollback::serde::Serialize, ::rollback::serde::Deserialize, ::std::clone::Clone, ::borrow::Partial, ::std::hash::Hash)] },
                );
                item_struct.attrs.push(parse_quote! {#[module(crate)]});

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
    let iter_path_string = paths
        .iter()
        .map(|f| f.0.clone().to_token_stream().to_string())
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
                    let mut local = self.value.log.lock().unwrap();
                    let trans = self.value.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    local.push_back(Box::new(undo));
                    global.log.push_back((trans, self.value.field, self.hash));
                }
            }
        },
    ));

    items.push(item(
        "struct Undo<T>",
        quote! {
            #[derive(::core::default::Default, ::rollback::Debug, ::serde::Serialize, ::serde::Deserialize, ::std::clone::Clone)]
            pub struct Undo<T> where T: ::core::default::Default + ::std::clone::Clone + ::serde::Serialize + ::std::marker::Send + ::std::hash::Hash + 'static {
                #[serde(skip)]
                #[debug(skip)]
                log: ::std::sync::Arc<::std::sync::Mutex<::std::collections::VecDeque<Box<dyn FnOnce(&mut T, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>>>>,
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
                    let mut local = self.log.lock().unwrap();
                    let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
                    local.push_back(Box::new(undo));
                    let hash = unsafe {self.hash_data()};
                    global.log.push_back((trans, self.field, hash));
                }

                pub fn delayed_undo(&mut self) -> DelayedUndo<T, '_> {
                    DelayedUndo {
                        hash: unsafe { self.hash_data() },
                        value: self
                    }
                }

                pub unsafe fn hash_data(&self) -> u32 {
                    ::crc32fast::hash(&::bincode::serialize(&self.data).unwrap()[..])
                }

                pub fn print_log(&mut self) {
                    ::log::info!("{:?}", self.global_log.lock().unwrap().log);
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
        "RollbackLog",
        quote! {
            #[derive(::core::default::Default)]
            pub struct RollbackLog {
                pub log: ::std::collections::VecDeque<(usize, usize, u32)>,
                pub client: ::std::option::Option<::crossbeam::channel::Sender<crate::GameDataUpdate>>,
                info: ::rollback::RollbackInfo,
                #(#iter_log_ident1 : ::std::sync::Arc<::std::sync::Mutex<::std::collections::VecDeque<Box<dyn FnOnce(&mut #iter_ty1, &::crossbeam::channel::Sender<crate::GameDataUpdate>) + Send>>>>,)*
            }
        },
    ));

    items.push(item(
        "Rollback",
        quote! {
            #[derive(::serde::Serialize, ::serde::Deserialize, ::std::clone::Clone, ::borrow::Partial, ::rollback::Debug)]
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

    let iter_log_ident_index1 = iter_log_ident_index.clone();
    let iter_log_ident_index2 = iter_log_ident_index.clone();
    let iter_log_ident1 = iter_log_ident.clone();
    let iter_log_ident2 = iter_log_ident.clone();
    let iter_path1 = iter_path.clone();
    let iter_log_ident_index2 = iter_log_ident_index.clone();
    let iter_log_ident2 = iter_log_ident.clone();
    let iter_path2 = iter_path.clone();
    let iter_path3 = iter_path.clone();
    let iter_path4 = iter_path.clone();
    let iter_path5 = iter_path.clone();
    let iter_path6 = iter_path.clone();
    let iter_path7 = iter_path.clone();
    let iter_path_string1 = iter_path_string.clone();
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
                    let oldest = rollback_log.info.oldest.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    while let Some((transaction, field, hash)) = rollback_log.log.pop_back().clone() {
                        if current == transaction {
                            match field {
                                #(#iter_log_ident_index1 => {
                                    let func = rollback_log.#iter_log_ident1.lock().unwrap().pop_back().unwrap();
                                    let previous = unsafe { self.#iter_path4.hash_data() };
                                    let previous_data = self.#iter_path5.data.clone();
                                    func(&mut self.#iter_path1.data, rollback_log.client.as_ref().unwrap());
                                    let new_hash = unsafe { self.#iter_path6.hash_data() };
                                    if new_hash != hash {
                                        println!("Hash verification failed for self.{}.hash_data() in transaction {:?} with new_hash != hash: {:?} != {:?}\n hash before undo = {:?} \nlog: {:?}", #iter_path_string1, transaction, new_hash, hash, previous, rollback_log.log);
                                        match ::assert_json_diff::assert_json_matches_no_panic(&self.#iter_path7.data, &previous_data, ::assert_json_diff::Config::new(::assert_json_diff::CompareMode::Strict)) {
                                            Ok(()) => panic!("Before and after is equal via serde_json"),
                                            Err(e) => panic!("lhs: new, rhs: old. {}", e),
                                        }
                                    }
                                })*
                                _ => panic!("Tried to undo field that doesn't exist.")
                            }
                        } else {
                            rollback_log.log.push_back((transaction, field, hash));
                            break;
                        }
                    }
                    rollback_log.info.current.store(current - 1, ::std::sync::atomic::Ordering::SeqCst).clone();
                }
                pub fn forget(&mut self) {
                    use ::std::hash::{Hash, Hasher};
                    let rollback_log = self.log.clone();
                    let mut rollback_log = rollback_log.lock().unwrap();
                    let oldest = rollback_log.info.oldest.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    let current = rollback_log.info.current.load(::std::sync::atomic::Ordering::SeqCst).clone();
                    if oldest >= current {
                        panic!("Cannot forget transaction or transaction that doesn't exist. oldest, current = {:?}, {:?}", oldest, current);
                    }
                    let mut forgot = false;
                    while let Some((transaction, field, hash)) = rollback_log.log.pop_front().clone() {
                        if oldest + 1 < transaction {
                            rollback_log.log.push_front((transaction, field, hash));
                            break;
                        }
                        forgot = true;
                        match field {
                            #(#iter_log_ident_index2 => {
                                rollback_log.#iter_log_ident2.lock().unwrap().pop_front().unwrap();
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
                pub fn reinitialize(&mut self, client: ::std::option::Option<::crossbeam::channel::Sender<crate::GameDataUpdate>>) {
                    let mut log = RollbackLog::default();
                    log.client = client;
                    let log = ::std::sync::Arc::new(::std::sync::Mutex::new(log));
                    self.log = log.clone();
                    self.data.global_log = log.clone();
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
