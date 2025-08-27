//! Issues:
//! - tons of functions in trait implementation that make the abstraction very leaky
//!   - to fix - i have no idea.
//! - Rollback has to implement Default but needs to be initialized with new
//!   - to fix - derive an implementation of Default in the macro alongside Rollback

// use std::{
//     collections::VecDeque,
//     sync::{Arc, Mutex, atomic::AtomicUsize},
// };

pub use derive_more::Debug;
pub use macros::{RollbackDerive, rollback};
pub use serde;

// type Transaction = usize;
// type Remaining = usize;
// type Order = usize;
// type EventLog<T> = VecDeque<(Transaction, Remaining, Order, Box<dyn Fn(&mut T)>)>;

// #[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
// pub struct Undo<T>
// where
//     T: Default + 'static,
// {
//     #[debug(skip)]
//     #[serde(skip)]
//     log: Arc<Mutex<EventLog<T>>>,
//     #[debug(skip)]
//     #[serde(skip)]
//     current: Arc<AtomicUsize>,
//     #[debug(skip)]
//     #[serde(skip)]
//     changed: usize,
//     #[debug(skip)]
//     #[serde(skip)]
//     order: Arc<AtomicUsize>,
//     #[debug(skip)]
//     #[serde(skip)]
//     oldest: Arc<AtomicUsize>,
//     pub data: T,
// }

// impl<T> Undo<T>
// where
//     T: Default + 'static,
// {
//     pub fn _undo(&mut self, func: Change<T>) {
//         func(&mut self.data);
//     }

//     pub fn _new_transaction(&mut self) {
//         self.changed = 0;
//     }

//     pub fn _increment_global(&mut self) {
//         self.order.store(0, Ordering::SeqCst);
//         self.current_transaction
//             .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
//     }

//     pub fn _set_data(
//         &mut self,
//         order: Arc<AtomicUsize>,
//         transaction: Arc<AtomicUsize>,
//         oldest: Arc<AtomicUsize>,
//     ) {
//         self.order = order;
//         self.current_transaction = transaction;
//         self.oldest_transaction = oldest;
//     }

//     pub fn _current_transaction(&self) -> usize {
//         self.current_transaction.load(Ordering::SeqCst)
//     }

//     pub fn _oldest_transaction(&self) -> usize {
//         self.oldest_transaction.load(Ordering::SeqCst)
//     }

//     pub fn _update_oldest_after_forget(&mut self) {
//         let oldest = self.oldest_transaction.load(Ordering::SeqCst);
//         let current = self.current_transaction.load(Ordering::SeqCst);
//         if oldest > current {
//             // Nothing to forget.
//             return;
//         } else {
//             self.oldest_transaction.fetch_add(1, Ordering::SeqCst);
//         }
//     }

//     pub fn _pop_undo_stack_if_changed(&mut self) -> Option<EventLog<T>> {
//         let (transaction, remaining, _, _) = if let Some(f) = self.log.iter().last() {
//             f
//         } else {
//             return None;
//         };

//         // println!(
//         //     "{:?} != {:?}",
//         //     transaction,
//         //     self.current_transaction.load(Ordering::SeqCst)
//         // );
//         if *transaction != self.current_transaction.load(Ordering::SeqCst) {
//             return None;
//         }
//         // println!(
//         //     "len {:?}, at {:?}",
//         //     self.log.len(),
//         //     self.log.len() - (*remaining + 1)
//         // );
//         let logs = self.log.split_off(self.log.len() - (*remaining + 1));
//         // for (a, b, c, _) in logs.iter() {
//         //     println!("{:?} {:?} {:?}", a, b, c);
//         // }
//         Some(logs)
//     }

//     fn _get_order(&self) -> Arc<AtomicUsize> {
//         self.order.clone()
//     }

//     pub fn _forget_last(&mut self) {
//         let transaction_to_forget = self.oldest_transaction.load(Ordering::SeqCst);
//         while let Some(t) = self.log.pop_front() {
//             if t.0 <= transaction_to_forget {
//                 continue;
//             } else {
//                 self.log.push_front(t);
//                 break;
//             }
//         }
//     }
//     pub fn _init_mut(&mut self) -> &mut T {
//         &mut self.data
//     }
// }

#[derive(Debug)]
pub struct ChangeInfo {
    pub depth: Vec<usize>,
    pub field: usize,
    pub order: usize,
}

impl Ord for ChangeInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.order.cmp(&other.order)
    }
}

impl PartialOrd for ChangeInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.order.partial_cmp(&other.order)
    }
}

impl PartialEq for ChangeInfo {
    fn eq(&self, other: &Self) -> bool {
        self.order.eq(&other.order)
    }
}

impl Eq for ChangeInfo {}

impl ChangeInfo {
    pub fn new(depth: Vec<usize>, field: usize, order: usize) -> Self {
        Self {
            depth,
            field,
            order,
        }
    }
}

pub trait Rollback: Default + serde::Serialize + for<'a> serde::Deserialize<'a> {
    fn new_transaction(&mut self);
    fn forget(&mut self);
    fn rollback(&mut self);
    fn current(&mut self) -> usize;
    fn oldest(&mut self) -> usize;
}
