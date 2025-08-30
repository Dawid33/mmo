#![feature(prelude_import)]
#![allow(unused)]
#[prelude_import]
use std::prelude::rust_2024::*;
#[macro_use]
extern crate std;
use rollback::rollback;
use crate::mod_test::Test;
mod mod_test {
    pub struct Test {
        pub tick: Undo<usize>,
    }
    #[automatically_derived]
    impl ::core::default::Default for Test {
        #[inline]
        fn default() -> Test {
            Test {
                tick: ::core::default::Default::default(),
            }
        }
    }
    #[allow(unreachable_code)]
    #[automatically_derived]
    impl derive_more::core::fmt::Debug for Test {
        #[inline]
        fn fmt(
            &self,
            __derive_more_f: &mut derive_more::core::fmt::Formatter<'_>,
        ) -> derive_more::core::fmt::Result {
            let tick = &self.tick;
            derive_more::core::fmt::DebugStruct::finish(
                derive_more::core::fmt::DebugStruct::field(
                    &mut derive_more::core::fmt::Formatter::debug_struct(
                        __derive_more_f,
                        "Test",
                    ),
                    "tick",
                    &tick,
                ),
            )
        }
    }
    #[doc(hidden)]
    #[allow(
        non_upper_case_globals,
        unused_attributes,
        unused_qualifications,
        clippy::absolute_paths,
    )]
    const _: () = {
        #[allow(unused_extern_crates, clippy::useless_attribute)]
        extern crate serde as _serde;
        #[automatically_derived]
        impl _serde::Serialize for Test {
            fn serialize<__S>(
                &self,
                __serializer: __S,
            ) -> _serde::__private::Result<__S::Ok, __S::Error>
            where
                __S: _serde::Serializer,
            {
                let mut __serde_state = _serde::Serializer::serialize_struct(
                    __serializer,
                    "Test",
                    false as usize + 1,
                )?;
                _serde::ser::SerializeStruct::serialize_field(
                    &mut __serde_state,
                    "tick",
                    &self.tick,
                )?;
                _serde::ser::SerializeStruct::end(__serde_state)
            }
        }
    };
    #[doc(hidden)]
    #[allow(
        non_upper_case_globals,
        unused_attributes,
        unused_qualifications,
        clippy::absolute_paths,
    )]
    const _: () = {
        #[allow(unused_extern_crates, clippy::useless_attribute)]
        extern crate serde as _serde;
        #[automatically_derived]
        impl<'de> _serde::Deserialize<'de> for Test {
            fn deserialize<__D>(
                __deserializer: __D,
            ) -> _serde::__private::Result<Self, __D::Error>
            where
                __D: _serde::Deserializer<'de>,
            {
                #[allow(non_camel_case_types)]
                #[doc(hidden)]
                enum __Field {
                    __field0,
                    __ignore,
                }
                #[doc(hidden)]
                struct __FieldVisitor;
                #[automatically_derived]
                impl<'de> _serde::de::Visitor<'de> for __FieldVisitor {
                    type Value = __Field;
                    fn expecting(
                        &self,
                        __formatter: &mut _serde::__private::Formatter,
                    ) -> _serde::__private::fmt::Result {
                        _serde::__private::Formatter::write_str(
                            __formatter,
                            "field identifier",
                        )
                    }
                    fn visit_u64<__E>(
                        self,
                        __value: u64,
                    ) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        match __value {
                            0u64 => _serde::__private::Ok(__Field::__field0),
                            _ => _serde::__private::Ok(__Field::__ignore),
                        }
                    }
                    fn visit_str<__E>(
                        self,
                        __value: &str,
                    ) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        match __value {
                            "tick" => _serde::__private::Ok(__Field::__field0),
                            _ => _serde::__private::Ok(__Field::__ignore),
                        }
                    }
                    fn visit_bytes<__E>(
                        self,
                        __value: &[u8],
                    ) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        match __value {
                            b"tick" => _serde::__private::Ok(__Field::__field0),
                            _ => _serde::__private::Ok(__Field::__ignore),
                        }
                    }
                }
                #[automatically_derived]
                impl<'de> _serde::Deserialize<'de> for __Field {
                    #[inline]
                    fn deserialize<__D>(
                        __deserializer: __D,
                    ) -> _serde::__private::Result<Self, __D::Error>
                    where
                        __D: _serde::Deserializer<'de>,
                    {
                        _serde::Deserializer::deserialize_identifier(
                            __deserializer,
                            __FieldVisitor,
                        )
                    }
                }
                #[doc(hidden)]
                struct __Visitor<'de> {
                    marker: _serde::__private::PhantomData<Test>,
                    lifetime: _serde::__private::PhantomData<&'de ()>,
                }
                #[automatically_derived]
                impl<'de> _serde::de::Visitor<'de> for __Visitor<'de> {
                    type Value = Test;
                    fn expecting(
                        &self,
                        __formatter: &mut _serde::__private::Formatter,
                    ) -> _serde::__private::fmt::Result {
                        _serde::__private::Formatter::write_str(
                            __formatter,
                            "struct Test",
                        )
                    }
                    #[inline]
                    fn visit_seq<__A>(
                        self,
                        mut __seq: __A,
                    ) -> _serde::__private::Result<Self::Value, __A::Error>
                    where
                        __A: _serde::de::SeqAccess<'de>,
                    {
                        let __field0 = match _serde::de::SeqAccess::next_element::<
                            Undo<usize>,
                        >(&mut __seq)? {
                            _serde::__private::Some(__value) => __value,
                            _serde::__private::None => {
                                return _serde::__private::Err(
                                    _serde::de::Error::invalid_length(
                                        0usize,
                                        &"struct Test with 1 element",
                                    ),
                                );
                            }
                        };
                        _serde::__private::Ok(Test { tick: __field0 })
                    }
                    #[inline]
                    fn visit_map<__A>(
                        self,
                        mut __map: __A,
                    ) -> _serde::__private::Result<Self::Value, __A::Error>
                    where
                        __A: _serde::de::MapAccess<'de>,
                    {
                        let mut __field0: _serde::__private::Option<Undo<usize>> = _serde::__private::None;
                        while let _serde::__private::Some(__key) = _serde::de::MapAccess::next_key::<
                            __Field,
                        >(&mut __map)? {
                            match __key {
                                __Field::__field0 => {
                                    if _serde::__private::Option::is_some(&__field0) {
                                        return _serde::__private::Err(
                                            <__A::Error as _serde::de::Error>::duplicate_field("tick"),
                                        );
                                    }
                                    __field0 = _serde::__private::Some(
                                        _serde::de::MapAccess::next_value::<
                                            Undo<usize>,
                                        >(&mut __map)?,
                                    );
                                }
                                _ => {
                                    let _ = _serde::de::MapAccess::next_value::<
                                        _serde::de::IgnoredAny,
                                    >(&mut __map)?;
                                }
                            }
                        }
                        let __field0 = match __field0 {
                            _serde::__private::Some(__field0) => __field0,
                            _serde::__private::None => {
                                _serde::__private::de::missing_field("tick")?
                            }
                        };
                        _serde::__private::Ok(Test { tick: __field0 })
                    }
                }
                #[doc(hidden)]
                const FIELDS: &'static [&'static str] = &["tick"];
                _serde::Deserializer::deserialize_struct(
                    __deserializer,
                    "Test",
                    FIELDS,
                    __Visitor {
                        marker: _serde::__private::PhantomData::<Test>,
                        lifetime: _serde::__private::PhantomData,
                    },
                )
            }
        }
    };
    #[automatically_derived]
    impl ::core::clone::Clone for Test {
        #[inline]
        fn clone(&self) -> Test {
            Test {
                tick: ::core::clone::Clone::clone(&self.tick),
            }
        }
    }
    impl<T: ::core::default::Default> AsRef<T> for Undo<T> {
        fn as_ref(&self) -> &T {
            &self.data
        }
    }
    impl<T> ::std::ops::Deref for Undo<T>
    where
        T: Default,
    {
        type Target = T;
        fn deref(&self) -> &Self::Target {
            &self.data
        }
    }
    impl<T> ::std::ops::DerefMut for Undo<T>
    where
        T: Default,
    {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.data
        }
    }
    impl ::std::ops::Deref for Rollback {
        type Target = Test;
        fn deref(&self) -> &Self::Target {
            &self.data
        }
    }
    impl ::std::ops::DerefMut for Rollback {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.data
        }
    }
    pub struct Undo<T>
    where
        T: Default + 'static,
    {
        #[serde(skip)]
        #[debug(skip)]
        log: ::std::sync::Arc<
            ::std::sync::Mutex<::std::collections::VecDeque<Box<dyn Fn(&mut T)>>>,
        >,
        #[serde(skip)]
        #[debug(skip)]
        global_log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
        #[serde(skip)]
        #[debug(skip)]
        info: ::rollback::RollbackInfo,
        #[serde(skip)]
        #[debug(skip)]
        field: usize,
        #[serde(skip)]
        #[debug(skip)]
        data: T,
    }
    #[automatically_derived]
    impl<T: ::core::default::Default> ::core::default::Default for Undo<T>
    where
        T: Default + 'static,
    {
        #[inline]
        fn default() -> Undo<T> {
            Undo {
                log: ::core::default::Default::default(),
                global_log: ::core::default::Default::default(),
                info: ::core::default::Default::default(),
                field: ::core::default::Default::default(),
                data: ::core::default::Default::default(),
            }
        }
    }
    #[allow(unreachable_code)]
    #[automatically_derived]
    impl<T> derive_more::core::fmt::Debug for Undo<T>
    where
        T: Default + 'static,
    {
        #[inline]
        fn fmt(
            &self,
            __derive_more_f: &mut derive_more::core::fmt::Formatter<'_>,
        ) -> derive_more::core::fmt::Result {
            let log = &self.log;
            let global_log = &self.global_log;
            let info = &self.info;
            let field = &self.field;
            let data = &self.data;
            derive_more::core::fmt::DebugStruct::finish_non_exhaustive(
                &mut derive_more::core::fmt::Formatter::debug_struct(
                    __derive_more_f,
                    "Undo",
                ),
            )
        }
    }
    #[doc(hidden)]
    #[allow(
        non_upper_case_globals,
        unused_attributes,
        unused_qualifications,
        clippy::absolute_paths,
    )]
    const _: () = {
        #[allow(unused_extern_crates, clippy::useless_attribute)]
        extern crate serde as _serde;
        #[automatically_derived]
        impl<T> _serde::Serialize for Undo<T>
        where
            T: Default + 'static,
        {
            fn serialize<__S>(
                &self,
                __serializer: __S,
            ) -> _serde::__private::Result<__S::Ok, __S::Error>
            where
                __S: _serde::Serializer,
            {
                let __serde_state = _serde::Serializer::serialize_struct(
                    __serializer,
                    "Undo",
                    false as usize,
                )?;
                _serde::ser::SerializeStruct::end(__serde_state)
            }
        }
    };
    #[doc(hidden)]
    #[allow(
        non_upper_case_globals,
        unused_attributes,
        unused_qualifications,
        clippy::absolute_paths,
    )]
    const _: () = {
        #[allow(unused_extern_crates, clippy::useless_attribute)]
        extern crate serde as _serde;
        #[automatically_derived]
        impl<'de, T> _serde::Deserialize<'de> for Undo<T>
        where
            T: Default + 'static,
            T: _serde::__private::Default,
        {
            fn deserialize<__D>(
                __deserializer: __D,
            ) -> _serde::__private::Result<Self, __D::Error>
            where
                __D: _serde::Deserializer<'de>,
            {
                #[allow(non_camel_case_types)]
                #[doc(hidden)]
                enum __Field {
                    __ignore,
                }
                #[doc(hidden)]
                struct __FieldVisitor;
                #[automatically_derived]
                impl<'de> _serde::de::Visitor<'de> for __FieldVisitor {
                    type Value = __Field;
                    fn expecting(
                        &self,
                        __formatter: &mut _serde::__private::Formatter,
                    ) -> _serde::__private::fmt::Result {
                        _serde::__private::Formatter::write_str(
                            __formatter,
                            "field identifier",
                        )
                    }
                    fn visit_u64<__E>(
                        self,
                        __value: u64,
                    ) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        match __value {
                            _ => _serde::__private::Ok(__Field::__ignore),
                        }
                    }
                    fn visit_str<__E>(
                        self,
                        __value: &str,
                    ) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        match __value {
                            _ => _serde::__private::Ok(__Field::__ignore),
                        }
                    }
                    fn visit_bytes<__E>(
                        self,
                        __value: &[u8],
                    ) -> _serde::__private::Result<Self::Value, __E>
                    where
                        __E: _serde::de::Error,
                    {
                        match __value {
                            _ => _serde::__private::Ok(__Field::__ignore),
                        }
                    }
                }
                #[automatically_derived]
                impl<'de> _serde::Deserialize<'de> for __Field {
                    #[inline]
                    fn deserialize<__D>(
                        __deserializer: __D,
                    ) -> _serde::__private::Result<Self, __D::Error>
                    where
                        __D: _serde::Deserializer<'de>,
                    {
                        _serde::Deserializer::deserialize_identifier(
                            __deserializer,
                            __FieldVisitor,
                        )
                    }
                }
                #[doc(hidden)]
                struct __Visitor<'de, T>
                where
                    T: Default + 'static,
                    T: _serde::__private::Default,
                {
                    marker: _serde::__private::PhantomData<Undo<T>>,
                    lifetime: _serde::__private::PhantomData<&'de ()>,
                }
                #[automatically_derived]
                impl<'de, T> _serde::de::Visitor<'de> for __Visitor<'de, T>
                where
                    T: Default + 'static,
                    T: _serde::__private::Default,
                {
                    type Value = Undo<T>;
                    fn expecting(
                        &self,
                        __formatter: &mut _serde::__private::Formatter,
                    ) -> _serde::__private::fmt::Result {
                        _serde::__private::Formatter::write_str(
                            __formatter,
                            "struct Undo",
                        )
                    }
                    #[inline]
                    fn visit_seq<__A>(
                        self,
                        _: __A,
                    ) -> _serde::__private::Result<Self::Value, __A::Error>
                    where
                        __A: _serde::de::SeqAccess<'de>,
                    {
                        let __field0 = _serde::__private::Default::default();
                        let __field1 = _serde::__private::Default::default();
                        let __field2 = _serde::__private::Default::default();
                        let __field3 = _serde::__private::Default::default();
                        let __field4 = _serde::__private::Default::default();
                        _serde::__private::Ok(Undo {
                            log: __field0,
                            global_log: __field1,
                            info: __field2,
                            field: __field3,
                            data: __field4,
                        })
                    }
                    #[inline]
                    fn visit_map<__A>(
                        self,
                        mut __map: __A,
                    ) -> _serde::__private::Result<Self::Value, __A::Error>
                    where
                        __A: _serde::de::MapAccess<'de>,
                    {
                        while let _serde::__private::Some(__key) = _serde::de::MapAccess::next_key::<
                            __Field,
                        >(&mut __map)? {
                            match __key {
                                _ => {
                                    let _ = _serde::de::MapAccess::next_value::<
                                        _serde::de::IgnoredAny,
                                    >(&mut __map)?;
                                }
                            }
                        }
                        _serde::__private::Ok(Undo {
                            log: _serde::__private::Default::default(),
                            global_log: _serde::__private::Default::default(),
                            info: _serde::__private::Default::default(),
                            field: _serde::__private::Default::default(),
                            data: _serde::__private::Default::default(),
                        })
                    }
                }
                #[doc(hidden)]
                const FIELDS: &'static [&'static str] = &[];
                _serde::Deserializer::deserialize_struct(
                    __deserializer,
                    "Undo",
                    FIELDS,
                    __Visitor {
                        marker: _serde::__private::PhantomData::<Undo<T>>,
                        lifetime: _serde::__private::PhantomData,
                    },
                )
            }
        }
    };
    #[automatically_derived]
    impl<T: ::core::clone::Clone> ::core::clone::Clone for Undo<T>
    where
        T: Default + 'static,
    {
        #[inline]
        fn clone(&self) -> Undo<T> {
            Undo {
                log: ::core::clone::Clone::clone(&self.log),
                global_log: ::core::clone::Clone::clone(&self.global_log),
                info: ::core::clone::Clone::clone(&self.info),
                field: ::core::clone::Clone::clone(&self.field),
                data: ::core::clone::Clone::clone(&self.data),
            }
        }
    }
    impl<T> Undo<T>
    where
        T: Default + 'static,
    {
        pub fn undo(&mut self, undo: impl Fn(&mut T) + 'static) {
            let mut global = self.global_log.lock().unwrap();
            let mut local = self.log.lock().unwrap();
            let trans = self.info.current.load(::std::sync::atomic::Ordering::SeqCst);
            local.push_back(Box::new(undo));
            global.log.push_back((trans, self.field));
            &mut self.data
        }
    }
    pub struct RollbackLog {
        log: ::std::collections::VecDeque<(usize, usize)>,
        info: ::rollback::RollbackInfo,
        tick0: ::std::sync::Arc<
            ::std::sync::Mutex<::std::collections::VecDeque<Box<dyn Fn(&mut usize)>>>,
        >,
    }
    #[automatically_derived]
    impl ::core::default::Default for RollbackLog {
        #[inline]
        fn default() -> RollbackLog {
            RollbackLog {
                log: ::core::default::Default::default(),
                info: ::core::default::Default::default(),
                tick0: ::core::default::Default::default(),
            }
        }
    }
    pub struct Rollback {
        log: ::std::sync::Arc<::std::sync::Mutex<RollbackLog>>,
        data: Undo<Test>,
    }
    impl Rollback {
        pub fn current(&self) -> usize {
            self.log
                .lock()
                .unwrap()
                .info
                .current
                .load(::std::sync::atomic::Ordering::SeqCst)
        }
        pub fn oldest(&self) -> usize {
            self.log
                .lock()
                .unwrap()
                .info
                .oldest
                .load(::std::sync::atomic::Ordering::SeqCst)
        }
        pub fn new_transaction(&mut self) {
            self.log
                .lock()
                .unwrap()
                .info
                .current
                .fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
        }
        pub fn rollback(&mut self) {
            let rollback_log = self.log.clone();
            let mut rollback_log = rollback_log.lock().unwrap();
            let current = rollback_log
                .info
                .current
                .load(::std::sync::atomic::Ordering::SeqCst)
                .clone();
            while let Some((transaction, field)) = rollback_log.log.pop_back().clone() {
                if current == transaction {
                    match field {
                        0usize => {
                            let func = rollback_log
                                .tick0
                                .lock()
                                .unwrap()
                                .pop_back()
                                .unwrap();
                            func(&mut self.tick.data);
                        }
                        _ => {
                            ::core::panicking::panic_fmt(
                                format_args!("Tried to undo field that doesn\'t exist."),
                            );
                        }
                    }
                } else {
                    rollback_log.log.push_back((transaction, field));
                    break;
                }
            }
            rollback_log
                .info
                .current
                .store(current - 1, ::std::sync::atomic::Ordering::SeqCst)
                .clone();
        }
        pub fn forget(&mut self) {
            let rollback_log = self.log.clone();
            let mut rollback_log = rollback_log.lock().unwrap();
            let oldest = rollback_log
                .info
                .oldest
                .load(::std::sync::atomic::Ordering::SeqCst)
                .clone();
            let current = rollback_log
                .info
                .current
                .load(::std::sync::atomic::Ordering::SeqCst)
                .clone();
            if oldest >= current {
                return;
            }
            while let Some((transaction, field)) = rollback_log.log.pop_front().clone() {
                if oldest != transaction {
                    rollback_log.log.push_front((transaction, field));
                    break;
                }
            }
            rollback_log
                .info
                .oldest
                .store(oldest + 1, ::std::sync::atomic::Ordering::SeqCst)
                .clone();
        }
    }
    impl ::core::default::Default for Rollback {
        fn default() -> Self {
            use ::std::ops::DerefMut;
            let log = ::std::sync::Arc::new(
                ::std::sync::Mutex::new(RollbackLog::default()),
            );
            let mut r = Self {
                log: log.clone(),
                data: Undo::default(),
            };
            r.tick.global_log = log.clone();
            let mut log = log.lock().unwrap();
            r.tick.log = log.tick0.clone();
            r.tick.info = log.info.clone();
            r.tick.field = 0usize;
            drop(log);
            r
        }
    }
}
extern crate test;
#[rustc_test_marker = "transaction_increments"]
#[doc(hidden)]
pub const transaction_increments: test::TestDescAndFn = test::TestDescAndFn {
    desc: test::TestDesc {
        name: test::StaticTestName("transaction_increments"),
        ignore: false,
        ignore_message: ::core::option::Option::None,
        source_file: "crates/rollback/tests/simple.rs",
        start_line: 14usize,
        start_col: 8usize,
        end_line: 14usize,
        end_col: 30usize,
        compile_fail: false,
        no_run: false,
        should_panic: test::ShouldPanic::No,
        test_type: test::TestType::IntegrationTest,
    },
    testfn: test::StaticTestFn(
        #[coverage(off)]
        || test::assert_test_result(transaction_increments()),
    ),
};
pub fn transaction_increments() {
    let mut test = mod_test::Rollback::default();
    match (&test.current(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    test.new_transaction();
    match (&test.current(), &1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    test.new_transaction();
    match (&test.current(), &2) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
extern crate test;
#[rustc_test_marker = "as_mut"]
#[doc(hidden)]
pub const as_mut: test::TestDescAndFn = test::TestDescAndFn {
    desc: test::TestDesc {
        name: test::StaticTestName("as_mut"),
        ignore: false,
        ignore_message: ::core::option::Option::None,
        source_file: "crates/rollback/tests/simple.rs",
        start_line: 24usize,
        start_col: 8usize,
        end_line: 24usize,
        end_col: 14usize,
        compile_fail: false,
        no_run: false,
        should_panic: test::ShouldPanic::No,
        test_type: test::TestType::IntegrationTest,
    },
    testfn: test::StaticTestFn(#[coverage(off)] || test::assert_test_result(as_mut())),
};
pub fn as_mut() {
    let mut test = mod_test::Rollback::default();
    test.new_transaction();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    match (&test.tick.as_ref(), &&1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    test.rollback();
    match (&test.tick.as_ref(), &&0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
extern crate test;
#[rustc_test_marker = "two"]
#[doc(hidden)]
pub const two: test::TestDescAndFn = test::TestDescAndFn {
    desc: test::TestDesc {
        name: test::StaticTestName("two"),
        ignore: false,
        ignore_message: ::core::option::Option::None,
        source_file: "crates/rollback/tests/simple.rs",
        start_line: 35usize,
        start_col: 8usize,
        end_line: 35usize,
        end_col: 11usize,
        compile_fail: false,
        no_run: false,
        should_panic: test::ShouldPanic::No,
        test_type: test::TestType::IntegrationTest,
    },
    testfn: test::StaticTestFn(#[coverage(off)] || test::assert_test_result(two())),
};
pub fn two() {
    let mut test = mod_test::Rollback::default();
    test.new_transaction();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    test.new_transaction();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    match (&test.tick.as_ref(), &&2) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    test.rollback();
    match (&test.tick.as_ref(), &&1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
extern crate test;
#[rustc_test_marker = "forget_basic"]
#[doc(hidden)]
pub const forget_basic: test::TestDescAndFn = test::TestDescAndFn {
    desc: test::TestDesc {
        name: test::StaticTestName("forget_basic"),
        ignore: false,
        ignore_message: ::core::option::Option::None,
        source_file: "crates/rollback/tests/simple.rs",
        start_line: 49usize,
        start_col: 8usize,
        end_line: 49usize,
        end_col: 20usize,
        compile_fail: false,
        no_run: false,
        should_panic: test::ShouldPanic::No,
        test_type: test::TestType::IntegrationTest,
    },
    testfn: test::StaticTestFn(
        #[coverage(off)]
        || test::assert_test_result(forget_basic()),
    ),
};
pub fn forget_basic() {
    let mut test = mod_test::Rollback::default();
    match (&test.oldest(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    match (&test.current(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    test.forget();
    match (&test.oldest(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    match (&test.current(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
extern crate test;
#[rustc_test_marker = "forget"]
#[doc(hidden)]
pub const forget: test::TestDescAndFn = test::TestDescAndFn {
    desc: test::TestDesc {
        name: test::StaticTestName("forget"),
        ignore: false,
        ignore_message: ::core::option::Option::None,
        source_file: "crates/rollback/tests/simple.rs",
        start_line: 59usize,
        start_col: 8usize,
        end_line: 59usize,
        end_col: 14usize,
        compile_fail: false,
        no_run: false,
        should_panic: test::ShouldPanic::No,
        test_type: test::TestType::IntegrationTest,
    },
    testfn: test::StaticTestFn(#[coverage(off)] || test::assert_test_result(forget())),
};
pub fn forget() {
    let mut test = mod_test::Rollback::default();
    match (&test.oldest(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    match (&test.current(), &0) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    test.new_transaction();
    test.forget();
    match (&test.oldest(), &1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    match (&test.current(), &1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
extern crate test;
#[rustc_test_marker = "rollback_forgotten"]
#[doc(hidden)]
pub const rollback_forgotten: test::TestDescAndFn = test::TestDescAndFn {
    desc: test::TestDesc {
        name: test::StaticTestName("rollback_forgotten"),
        ignore: false,
        ignore_message: ::core::option::Option::None,
        source_file: "crates/rollback/tests/simple.rs",
        start_line: 70usize,
        start_col: 8usize,
        end_line: 70usize,
        end_col: 26usize,
        compile_fail: false,
        no_run: false,
        should_panic: test::ShouldPanic::No,
        test_type: test::TestType::IntegrationTest,
    },
    testfn: test::StaticTestFn(
        #[coverage(off)]
        || test::assert_test_result(rollback_forgotten()),
    ),
};
pub fn rollback_forgotten() {
    let mut test = mod_test::Rollback::default();
    let old = test.tick.as_ref().clone();
    test.tick.undo(move |d| *d = old);
    test.new_transaction();
    test.forget();
    test.rollback();
    match (&test.oldest(), &1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    match (&test.current(), &1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
    match (&test.tick.as_ref(), &&1) {
        (left_val, right_val) => {
            if !(*left_val == *right_val) {
                let kind = ::core::panicking::AssertKind::Eq;
                ::core::panicking::assert_failed(
                    kind,
                    &*left_val,
                    &*right_val,
                    ::core::option::Option::None,
                );
            }
        }
    };
}
#[rustc_main]
#[coverage(off)]
#[doc(hidden)]
pub fn main() -> () {
    extern crate test;
    test::test_main_static(
        &[
            &as_mut,
            &forget,
            &forget_basic,
            &rollback_forgotten,
            &transaction_increments,
            &two,
        ],
    )
}
