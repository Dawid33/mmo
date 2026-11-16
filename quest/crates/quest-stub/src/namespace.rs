//! The in-memory namespace store behind `world.*`.

use std::collections::BTreeMap;

use quest_vm::wire::WireValue;

/// An error value of the program's error enum: a variant naming the error.
pub(crate) fn error(name: &str, fields: Vec<(&str, WireValue)>) -> WireValue {
    WireValue::Variant {
        name: name.to_string(),
        fields: fields
            .into_iter()
            .map(|(n, v)| (n.to_string(), v))
            .collect(),
    }
}

pub(crate) type Answer = Result<WireValue, WireValue>;

#[derive(Clone, Debug, Default)]
pub(crate) struct Namespace {
    map: BTreeMap<String, WireValue>,
}

impl Namespace {
    pub(crate) fn current(&self, key: &str) -> Option<WireValue> {
        self.map.get(key).cloned()
    }

    pub(crate) fn get(&self, key: &str) -> Answer {
        self.current(key).ok_or_else(|| error("NotFound", vec![]))
    }

    pub(crate) fn set(&mut self, key: &str, v: WireValue) -> Answer {
        self.map.insert(key.to_string(), v);
        Ok(WireValue::Unit)
    }

    /// A missing key starts from 0; a non-int is `WrongType`; an overflow is `Lost` (nothing stored).
    pub(crate) fn add(&mut self, key: &str, n: i64) -> Answer {
        let base = match self.map.get(key) {
            None => 0,
            Some(WireValue::Int(i)) => *i,
            Some(_) => return Err(error("WrongType", vec![])),
        };
        let sum = base.checked_add(n).ok_or_else(|| error("Lost", vec![]))?;
        self.map.insert(key.to_string(), WireValue::Int(sum));
        Ok(WireValue::Int(sum))
    }

    /// A differing or missing current value is `Changed(current)` (`Unit` when missing).
    pub(crate) fn compare_and_set(
        &mut self,
        key: &str,
        expected: &WireValue,
        new: WireValue,
    ) -> Answer {
        match self.map.get(key) {
            Some(cur) if cur == expected => {
                self.map.insert(key.to_string(), new);
                Ok(WireValue::Unit)
            }
            cur => Err(error(
                "Changed",
                vec![("current", cur.cloned().unwrap_or_default())],
            )),
        }
    }

    pub(crate) fn delete(&mut self, key: &str) -> Answer {
        match self.map.remove(key) {
            Some(_) => Ok(WireValue::Unit),
            None => Err(error("NotFound", vec![])),
        }
    }
}
