//! The type table of a program (design D8): what the wire loader, the formatter and the compiler's
//! type-directed code agree on. Descriptors can come from a loaded program, so every query tolerates a
//! malformed table (an id out of range) instead of trusting it. A table may be cyclic: recursive data
//! types are allowed (decision 17), so nothing here recurses over the type graph.

use crate::fault::{FaultKind, VmResult};

pub type TypeId = u16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDesc {
    Unit,
    Bool,
    Int,
    Float,
    Str,
    List(TypeId),
    Dict(TypeId, TypeId),
    Promise(TypeId),
    Struct {
        name: String,
        fields: Vec<(String, TypeId)>,
    },
    /// `Option[T]` is `Enum { name: "Option", variants: [("None", []), ("Some", [("value", T)])] }`.
    /// A variant's tag is its index.
    Enum {
        name: String,
        variants: Vec<(String, Vec<(String, TypeId)>)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TypeTable(pub Vec<TypeDesc>);

impl TypeTable {
    /// The descriptor of `t`; an id outside the table is `Corrupt`.
    pub fn desc(&self, t: TypeId) -> VmResult<&TypeDesc> {
        self.0.get(usize::from(t)).ok_or(FaultKind::Corrupt)
    }

    /// Whether slots of this type hold heap kinds (a pointer, `0` or a field-less variant). An id
    /// outside the table is not a ref.
    pub fn is_ref(&self, t: TypeId) -> bool {
        matches!(
            self.0.get(usize::from(t)),
            Some(
                TypeDesc::Str
                    | TypeDesc::List(_)
                    | TypeDesc::Dict(..)
                    | TypeDesc::Struct { .. }
                    | TypeDesc::Enum { .. }
            )
        )
    }

    /// False when the type is or contains a `Promise` (it cannot be saved or sent), or names an id
    /// outside the table. A cyclic table terminates: each type id is visited once.
    pub fn storable(&self, t: TypeId) -> bool {
        let mut seen = vec![false; self.0.len()];
        let mut stack = vec![t];
        while let Some(t) = stack.pop() {
            let i = usize::from(t);
            let (Some(d), Some(mark)) = (self.0.get(i), seen.get_mut(i)) else {
                return false;
            };
            if std::mem::replace(mark, true) {
                continue;
            }
            match d {
                TypeDesc::Unit
                | TypeDesc::Bool
                | TypeDesc::Int
                | TypeDesc::Float
                | TypeDesc::Str => {}
                TypeDesc::Promise(_) => return false,
                TypeDesc::List(e) => stack.push(*e),
                TypeDesc::Dict(k, v) => stack.extend([*k, *v]),
                TypeDesc::Struct { fields, .. } => stack.extend(fields.iter().map(|(_, f)| *f)),
                TypeDesc::Enum { variants, .. } => stack.extend(
                    variants
                        .iter()
                        .flat_map(|(_, fs)| fs.iter().map(|(_, f)| *f)),
                ),
            }
        }
        true
    }
}
