//! The type table of a program (design D8): what the wire loader, the formatter and the compiler's
//! type-directed code agree on. Descriptors can come from a loaded program, so every query tolerates a
//! malformed table (an id out of range, a cycle) instead of trusting it.

use crate::consts::WIRE_DEPTH_MAX;
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

/// Descriptor visits one `storable` query may make: a table with shared sub-types can otherwise be
/// exponential to walk.
const STORABLE_VISITS_MAX: u32 = 4_096;

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

    /// False when the type is or contains a `Promise` (it cannot be saved or sent). A bad id, a cycle
    /// or a table too tangled to decide are also not storable.
    pub fn storable(&self, t: TypeId) -> bool {
        let mut visits = 0;
        self.storable_in(t, 0, &mut visits)
    }

    fn storable_in(&self, t: TypeId, depth: usize, visits: &mut u32) -> bool {
        *visits += 1;
        if depth > WIRE_DEPTH_MAX || *visits > STORABLE_VISITS_MAX {
            return false;
        }
        let Some(d) = self.0.get(usize::from(t)) else {
            return false;
        };
        let ids: Vec<TypeId> = match d {
            TypeDesc::Unit | TypeDesc::Bool | TypeDesc::Int | TypeDesc::Float | TypeDesc::Str => {
                vec![]
            }
            TypeDesc::Promise(_) => return false,
            TypeDesc::List(e) => vec![*e],
            TypeDesc::Dict(k, v) => vec![*k, *v],
            TypeDesc::Struct { fields, .. } => fields.iter().map(|(_, f)| *f).collect(),
            TypeDesc::Enum { variants, .. } => variants
                .iter()
                .flat_map(|(_, fs)| fs.iter().map(|(_, f)| *f))
                .collect(),
        };
        ids.into_iter()
            .all(|id| self.storable_in(id, depth + 1, visits))
    }
}
