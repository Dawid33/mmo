#![allow(unused)]
extern crate proc_macro;
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{quote, ToTokens};
use syn::{
    parse::{Parse, Parser}, parse_macro_input, parse_quote, punctuated::Punctuated, Attribute, DeriveInput, Ident, Meta, Visibility
};

#[proc_macro_derive(RollbackDerive, attributes(recurse, skip))]
pub fn rollback_derive(input: TokenStream) -> TokenStream {
    TokenStream::new()
}
/// Create struct that collects all data from fields
#[proc_macro_attribute]
pub fn rollback(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut ast = parse_macro_input!(input as DeriveInput);
    let name = &ast.ident;
    let module_ident = Ident::new(&format!("Module{}", name), Span::call_site());
    let rollback_log_ident = Ident::new(&format!("Rollback{}", name), Span::call_site());
    let undo_struct_ident = Ident::new(&format!("Undo{}", name), Span::call_site());
    let recurse = Ident::new("recurse", Span::call_site());
    let mut fields = Vec::new();
    let mut recurse_fields = Vec::new();

    match &mut ast.data {
        syn::Data::Struct(struct_data) => {
            match &mut struct_data.fields {
                syn::Fields::Named(named_fields) => {
                    let is_recurse = |attr: &[Attribute]| -> bool {
                        match attr.iter().find(|x| x.path().is_ident(&recurse)) {
                            Some(_) => true,
                            None => false,
                        }
                    };
                    recurse_fields = named_fields.named.clone().into_iter().filter_map(|f|
                        if is_recurse(&f.attrs) {
                             Some(f.clone())
                        } else {
                         None
                        }
                    ).collect();
                    fields = named_fields.named.clone().into_iter().collect();
                    for f in &mut named_fields.named {
                        let ty = &f.ty;
                        f.ty = syn::Type::parse
                            .parse2(quote! { #undo_struct_ident<#ty> })
                            .expect("Failed to change type of field to Undo<T>");
                        f.vis = syn::Visibility::parse
                            .parse2(quote! {pub}).unwrap();
                    }
                    named_fields.named.push(
                        syn::Field::parse_named
                            .parse2(quote! {#[serde(skip)] #[debug(skip)] rollback_log: #rollback_log_ident})
                            .expect("Bug: Failed to parse rollback_log field."),
                    );
                }
                syn::Fields::Unnamed(_) => {
                    panic!("rollback cannot have unnamed fields in a struct")
                }
                syn::Fields::Unit => panic!("rollback cannot have unit fields in a struct"),
            };
        }
        _ => panic!("rollback has to be used with structs"),
    }

    let fields_ident: Vec<syn::Ident> = fields
        .clone()
        .into_iter()
        .map(|f| f.ident.unwrap())
        .collect();
    let fields_ty: Vec<syn::Type> = fields.clone().into_iter().map(|f| f.ty).collect();
    let f_ident_index: Vec<usize> = fields_ident
        .clone()
        .into_iter()
        .enumerate()
        .map(|(i, _)| i)
        .collect();
    let f_recurse_ident : Vec<syn::Ident> = recurse_fields
        .clone()
        .into_iter()
        .map(|f| f.ident.unwrap())
        .collect();
    let f_recurse_ty : Vec<syn::Type> = recurse_fields.clone().into_iter().map(|f| f.ty).collect();
    let f_recurse_index: Vec<usize> = recurse_fields
        .clone()
        .into_iter()
        .enumerate()
        .map(|(i, _)| i)
        .collect();

    let f_ty1 = fields_ty.clone().into_iter();
    let f_ty2 = fields_ty.clone().into_iter();
    let f_ty3 = fields_ty.clone().into_iter();
    let f_ident_index1 = f_ident_index.clone().into_iter();
    let f_ident_index2 = f_ident_index.clone().into_iter();
    let f_recurse_ident1 = f_recurse_ident.clone().into_iter();
    let f_recurse_ident2 = f_recurse_ident.clone().into_iter();
    let f_recurse_ident3 = f_recurse_ident.clone().into_iter();
    let f_recurse_ident4 = f_recurse_ident.clone().into_iter();
    let f_recurse_ty1 = f_recurse_ty.clone().into_iter();
    let f_recurse_ty2 = f_recurse_ty.clone().into_iter();
    let f_recurse_ty3 = f_recurse_ty.clone().into_iter();
    let f_recurse_ty4 = f_recurse_ty.clone().into_iter();
    let f_recurse_index1 = f_recurse_index.clone().into_iter();
    let f_recurse_index2 = f_recurse_index.clone().into_iter();
    let f_ident1 = fields_ident.clone().into_iter();
    let f_ident2 = fields_ident.clone().into_iter();
    let f_ident3 = fields_ident.clone().into_iter();
    let f_ident4 = fields_ident.clone().into_iter();
    let f_ident5 = fields_ident.clone().into_iter();
    let f_ident6 = fields_ident.clone().into_iter();
    let f_ident7 = fields_ident.clone().into_iter();
    let f_ident8 = fields_ident.clone().into_iter();
    let f_ident9 = fields_ident.clone().into_iter();
    let f_ident10 = fields_ident.clone().into_iter();
    let f_ident11 = fields_ident.clone().into_iter();
    let f_ident12 = fields_ident.clone().into_iter();
    quote! {
        #[derive(Default, ::rollback::Debug, ::rollback::serde::Serialize, ::rollback::serde::Deserialize)]
        pub struct #undo_struct_ident<T>
        where
            T: Default + 'static,
        {
            #[debug(skip)]
            #[serde(skip)]
            log: ::std::sync::Arc<::std::sync::Mutex<::std::collections::VecDeque<(usize, usize, usize, Box<dyn Fn(&mut T)>)>>>,
            #[debug(skip)]
            #[serde(skip)]
            current: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
            #[debug(skip)]
            #[serde(skip)]
            order: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
            #[debug(skip)]
            #[serde(skip)]
            oldest: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
            #[debug(skip)]
            #[serde(skip)]
            changed: usize,
            data: T,
        }

        impl<T> ::std::ops::Deref for #undo_struct_ident<T>
        where T: Default {
            type Target = T;

            fn deref(&self) -> &Self::Target {
                &self.data
            }
        }

        impl<T> ::std::ops::DerefMut for #undo_struct_ident<T>
        where T: Default {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.data
            }
        }

        impl<T> #undo_struct_ident<T>
        where
            T: Default + serde::Serialize + for<'a> serde::Deserialize<'a>,
        {
            pub fn as_ref(&self) -> &T {
                &self.data
            }

            pub fn as_mut(&mut self, undo: Box<dyn Fn(&mut T)>) -> &mut T {
                let order = self.order.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
                let trans = self.order.load(::std::sync::atomic::Ordering::SeqCst);
                self.log
                    .lock()
                    .unwrap()
                    .push_back((trans, self.changed + 1, order, undo));
                self.changed += 1;
                &mut self.data
            }

            pub fn set(&mut self, new: T)
            where
                T: Clone,
            {
                let old = self.data.clone();
                let order = self.order.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let trans = self.current.load(std::sync::atomic::Ordering::SeqCst);
                self.log.lock().unwrap().push_back((
                    trans,
                    self.changed,
                    order,
                    Box::new(move |data| {
                        *data = old.clone();
                    }),
                ));
                self.changed += 1;
                self.data = new;
            }

            /// Do not call this function, it should only be used in the macro.
            pub fn _rollback(&mut self) {
                let (_, _, _, func) = self.log.lock().unwrap().pop_back().unwrap();
                func(&mut self.data);
            }
        }

        #[derive(::rollback::Debug, ::rollback::serde::Serialize, ::rollback::serde::Deserialize, ::rollback::RollbackDerive)]
        #ast

        #[derive(Default)]
        struct #rollback_log_ident {
            depth: Vec<usize>,
            current: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
            order: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
            oldest: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
            #(#f_ident1: ::std::sync::Arc<::std::sync::Mutex<::std::collections::VecDeque<(usize, usize, usize, Box<dyn Fn(&mut #f_ty1)>)>>>,)*
        }

        impl #rollback_log_ident {
            fn new_transaction(&mut self) {
                self.current.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
                self.order.store(0, ::std::sync::atomic::Ordering::SeqCst);
            }

            fn forget(&mut self) {
                
            }

            fn current(&mut self) -> usize {
                self.current.load(::std::sync::atomic::Ordering::SeqCst)
            }

            fn oldest(&mut self) -> usize {
                self.oldest.load(::std::sync::atomic::Ordering::SeqCst)
            }
        }

        impl #name {
            fn _init(
                parent: &mut Self, 
                current: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
                order: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
                oldest: ::std::sync::Arc<::std::sync::atomic::AtomicUsize>,
                depth: Vec<usize>
            ) {
                #(
                    let mut next_depth = depth.clone();
                    next_depth.push(#f_recurse_index1);
                    #f_recurse_ty2::_init(&mut parent.#f_recurse_ident2, current.clone(), order.clone(), oldest.clone(), next_depth);
                )*
                #(
                    parent.#f_ident12.current = current.clone();
                    parent.#f_ident12.order = order.clone();
                    parent.#f_ident12.oldest = oldest.clone();
                )*
                parent.rollback_log.current = current;
                parent.rollback_log.order = order;
                parent.rollback_log.oldest = oldest;
                parent.rollback_log.depth = depth;
            }

            fn _pre_rollback(mut parent: &mut Self, mut list: &mut ::std::collections::BinaryHeap<::rollback::ChangeInfo>) {
                #(#f_recurse_ty1::_pre_rollback(&mut parent.#f_recurse_ident1, &mut list);)*

                let transaction = parent.rollback_log.current.load(::std::sync::atomic::Ordering::SeqCst);
                #(
                    if parent.#f_ident9.changed != 0 {
                        for ((current, changed, order, _)) in parent.rollback_log.#f_ident9.lock().unwrap().iter().rev() {
                            if *current == transaction {
                                list.push(rollback::ChangeInfo::new(parent.rollback_log.depth.clone(), #f_ident_index1, *order));
                                parent.#f_ident9.changed -= 1;
                                continue;
                            }
                            break;
                        }
                    }
                )*
            }

            fn _undo(mut parent: &mut Self, mut info: rollback::ChangeInfo) {
                if info.depth.is_empty() {
                    match info.field {
                        #(#f_ident_index => parent.#f_ident11._rollback(),)*
                        _ => panic!("Bug: Tried to undo a field that doesn't exist."),
                    }
                } else {
                    if !info.depth.is_empty() {
                        info.depth.pop();
                    }
                    #(#f_recurse_ty4::_undo(&mut parent.#f_recurse_ident4, info);)*
                }
            }
        }

        impl ::rollback::Rollback for #name {
            fn new_transaction(&mut self) { self.rollback_log.new_transaction() }
            fn rollback(&mut self) {
                let mut list: ::std::collections::BinaryHeap<::rollback::ChangeInfo> = ::std::collections::BinaryHeap::new();
                Self::_pre_rollback(self, &mut list);
                while let Some(l) = list.pop() {
                    println!("{:?}", l);
                    Self::_undo(self, l);
                    Self::_pre_rollback(self, &mut list);
                }
            }
            fn forget(&mut self) { self.rollback_log.forget() }
            fn current(&mut self) -> usize { self.rollback_log.current() }
            fn oldest(&mut self) -> usize { self.rollback_log.oldest() }
        }

        impl Default for #name {
            fn default() -> Self {
                let order = ::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));
                let current = ::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));
                let oldest = ::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));

                #(let #f_ident3 = ::std::sync::Arc::new(::std::sync::Mutex::new(::std::collections::VecDeque::new()));)*
                // TODO: initialize recurse fields with arc atomic values
                let mut r = Self {
                    #(#f_ident4: #undo_struct_ident {
                        log: #f_ident5.clone(),
                        current: current.clone(),
                        changed: 0,
                        order: order.clone(),
                        oldest: oldest.clone(),
                        data: #f_ty2::default(),
                    },)*
                    rollback_log: #rollback_log_ident {
                        #(#f_ident6: #f_ident7.clone(),)*
                        current: current.clone(),
                        order: order.clone(),
                        oldest: oldest.clone(),
                        depth: Vec::new(),
                    },
                };
                #(
                    let depth = Vec::from([#f_recurse_index2]);
                    #f_recurse_ty3::_init(&mut r.#f_recurse_ident3, current.clone(), order.clone(), oldest.clone(), depth);
                )*
                r
            }
        }
    }
    .into()
}

//     let mut ast = parse_macro_input!(input as DeriveInput);
//     let name = ast.ident.clone();
//     let recurse = Ident::new("recurse", Span::call_site());
//     match &mut ast.data {
//         syn::Data::Struct(struct_data) => {
//             let mut new_transaction: Vec<proc_macro2::TokenStream> = Vec::new();
//             let mut undo_log_init: Vec<proc_macro2::TokenStream> = Vec::new();
//             let mut undo_log_gather: Vec<proc_macro2::TokenStream> = Vec::new();
//             let mut init_loop = None;
//             let mut init: Vec<proc_macro2::TokenStream> = Vec::new();
//             let mut rollback_loop: Vec<proc_macro2::TokenStream> = Vec::new();
//             let mut forget: Vec<proc_macro2::TokenStream> = Vec::new();
//             let mut current_transaction = None;
//             let mut oldest_transaction = None;
//             let mut forget_cleanup = None;
//             match &mut struct_data.fields {
//                 syn::Fields::Named(fields) => {
//                     let is_recurse = |attr: &[Attribute]| -> bool {
//                         match attr.iter().find(|x| x.path().is_ident(&recurse)) {
//                             Some(_) => true,
//                             None => false,
//                         }
//                     };

//                     let mut iter = fields.named.iter();
//                     if let Some(f) = iter.next() {
//                         let recurse = is_recurse(&f.attrs[..]);
//                         let f_name = &f.ident;
//                         let f_type = &f.ty;
//                         current_transaction = Some(quote! {
//                             return self.#f_name._current_transaction();
//                         });
//                         oldest_transaction = Some(quote! {
//                             return self.#f_name._oldest_transaction();
//                         });
//                         init.push(quote! {
//                             r.#f_name._set_data(order.clone(), trans.clone(), oldest.clone());
//                         });
//                         if recurse {
//                             init.push(quote! {
//                                 r.#f_name._init_mut()._new_inner(order.clone(), trans.clone(), oldest.clone());
//                             });
//                         }
//                         new_transaction.push(quote! {
//                             self.#f_name._increment_global();
//                             self.#f_name._new_transaction();
//                         });

//                         if recurse {
//                             undo_log_gather.push(quote! {
//                                 if self.#f_name._init_mut()._get_highest() > highest {
//                                     highest = self.#f_name._init_mut()._get_highest();
//                                 };
//                             });
//                         }
//                         undo_log_gather.push(quote! {
//                             let mut #f_name = if let Some(logs) = self.#f_name._pop_undo_stack_if_changed() {
//                                 for l in logs.iter() {
//                                     if l.2 > highest {
//                                         highest = l.2;
//                                     }
//                                 }
//                                 Some(logs)
//                             } else {
//                                 None
//                             };
//                         });
//                         init_loop = Some(quote! {
//                             let mut order = self.#f_name._get_order();
//                         });
//                         if recurse {
//                             rollback_loop.push(quote! {
//                                 highest = self.#f_name._init_mut()._try_rollback(highest);
//                             });
//                         }
//                         rollback_loop.push(quote! {
//                             #f_name = if let Some(mut logs) = #f_name {
//                                 if let Some((_, _, order, _)) = logs.iter().last() {
//                                     if *order == highest {
//                                         drop(order);
//                                         let (_,_,_,func) = logs.pop_back().unwrap();
//                                         self.#f_name._undo(func);
//                                         highest -= 1;
//                                     }
//                                     Some(logs)
//                                 } else {
//                                     None
//                                 }
//                             } else {
//                                 None
//                             };
//                         });
//                         forget_cleanup = Some(quote! {
//                             self.#f_name._update_oldest_after_forget();
//                         });
//                         if recurse {
//                             forget.push(quote! {
//                                 self.#f_name._init_mut().forget();
//                             });
//                         }
//                         forget.push(quote! {
//                             self.#f_name._forget_last();
//                         });
//                     };

//                     for f in iter {
//                         let recurse = is_recurse(&f.attrs[..]);
//                         let f_name = &f.ident;
//                         let f_type = &f.ty;
//                         init.push(quote! {
//                             r.#f_name._set_data(order.clone(), trans.clone(), oldest.clone());
//                         });
//                         if recurse {
//                             init.push(quote! {
//                                 r.#f_name._init_mut()._new_inner(order.clone(), trans.clone(), oldest.clone());
//                             });
//                         }
//                         new_transaction.push(quote! {
//                             self.#f_name._new_transaction();
//                         });
//                         if recurse {
//                             undo_log_gather.push(quote! {
//                                 if self.#f_name._init_mut()._get_highest() > highest {
//                                     highest = self.#f_name._init_mut()._get_highest();
//                                 };
//                             });
//                         }
//                         undo_log_gather.push(quote! {
//                             let mut #f_name = if let Some(logs) = self.#f_name._pop_undo_stack_if_changed() {
//                                 for l in logs.iter() {
//                                     if l.2 > highest {
//                                         highest = l.2;
//                                     }
//                                 }
//                                 Some(logs)
//                             } else {
//                                 None
//                             };
//                         });
//                         if recurse {
//                             rollback_loop.push(quote! {
//                                 highest = self.#f_name._init_mut()._try_rollback(highest);
//                             });
//                         }
//                         rollback_loop.push(quote! {
//                             #f_name = if let Some(mut logs) = #f_name {
//                                 if let Some((_, _, order, _)) = logs.iter().last() {
//                                     if *order == highest {
//                                         drop(order);
//                                         let (_,_,_,func) = logs.pop_back().unwrap();
//                                         self.#f_name._undo(func);
//                                         highest -= 1;
//                                     }
//                                     Some(logs)
//                                 } else {
//                                     None
//                                 }
//                             } else {
//                                 None
//                             };
//                         });
//                         if recurse {
//                             forget.push(quote! {
//                                 self.#f_name._init_mut().forget();
//                             });
//                         }
//                         forget.push(quote! {
//                             self.#f_name._forget_last();
//                         });
//                     }
//                 }
//                 _ => (),
//             }

//             let current_transaction =
//                 current_transaction.expect("Cannot derive Rollback macro for fieldless struct.");
//             let oldest_transaction =
//                 oldest_transaction.expect("Cannot derive Rollback macro for fieldless struct.");
//             let forget_cleanup = forget_cleanup.unwrap();
//             let init_loop = init_loop.unwrap();
//             let new_transaction = new_transaction.into_iter();
//             let undo_log_gather = undo_log_gather.into_iter();
//             let rollback_loop = rollback_loop.into_iter();
//             let forget = forget.into_iter();

//             let init_inner = init.clone();
//             let init = init.into_iter();
//             let init_inner = init_inner.into_iter();
//             return quote! {
//                 impl #name {
//                     fn _new_inner(
//                         &mut self,
//                         order: std::sync::Arc<std::sync::atomic::AtomicUsize>,
//                         trans: std::sync::Arc<std::sync::atomic::AtomicUsize>,
//                         oldest: std::sync::Arc<std::sync::atomic::AtomicUsize>
//                     ) {
//                         let mut r = self;
//                         #(#init_inner)*
//                     }

//                     fn _get_highest(&self) -> usize { 0 }
//                     fn _try_rollback(&mut self, highest: usize) -> usize { 0 }
//                 }
//                 impl rollback::RollbackTrait for #name {
//                     fn new() -> Self {
//                         let order = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
//                         let trans = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
//                         let oldest = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
//                         let mut r = #name::default();
//                         #(#init)*
//                         r
//                     }

//                     fn new_transaction(&mut self) {
//                         #(#new_transaction)*
//                     }

//                     fn current_transaction(&mut self) -> usize {
//                         #current_transaction
//                     }

//                     fn oldest_transaction(&mut self) -> usize {
//                         #oldest_transaction
//                     }

//                     fn rollback(&mut self) {
//                         let mut highest: usize = self._get_highest();
//                         #(#undo_log_gather)*
//                         while highest != 0 {
//                             #(#rollback_loop)*
//                         }
//                     }

//                     fn forget(&mut self) {
//                         #(#forget)*
//                         #forget_cleanup
//                     }
//                 }
//             }
//             .into();
//         }
//         _ => panic!("`add_field` has to be used with structs "),
//     }
// /// Create struct with fields representing undo logs of all sub fields
// #[proc_macro_derive(Rollback)]
// pub fn event_logged(item: TokenStream) -> TokenStream {
//     let ast = parse_macro_input!(item as DeriveInput);
//     let name = ast.ident;
//     quote! {}.into()
// }
