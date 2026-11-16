//! The instruction set (design D5). Op numbers, builtin ids, the fuel table and the memory layout are part
//! of [`BYTECODE_VERSION`]; tests pin each against literals and changing one means bumping the version.
//!
//! # Operands
//! `Instr { op, a, b, c }`. Slot operands are absolute word indexes into the static region. `T` is the
//! 32-bit value `b | c << 16`. Where the design leaves an operand unnamed, this is the fixed meaning
//! (the interpreter and the compiler both rely on it; [`Op::layout`] is what `validate` checks):
//!
//! | op | a | b | c |
//! |---|---|---|---|
//! | `GetField`, `TakeField` | destination | object slot | field index |
//! | `SetField` | object slot | field index | source slot (moved in) |
//! | `GetIndex`, `TakeIndex`, `GetKey`, `TakeKey` | destination | container slot | index / key slot |
//! | `SetIndex`, `SetKey` | container slot | index / key slot | source slot (moved in) |
//! | `RefMake` | destination | root slot | 1: the target is a heap kind |
//! | `RefField` | destination | base reference slot | field index |
//! | `RefIndex`, `RefKey` | destination | base reference slot | index / key slot |
//! | `RefLoad`, `RefTake` | destination | reference slot | - |
//! | `RefStore` | reference slot | source slot | - |
//! | `Await` | value (error in `a + 1`) | promise slot | expected type id |
//! | `AwaitAll` | results (error in `a + 1`) | slot of the list of promises | expected type id |
//! | `ListLen`, `GetTag`, `NegI`, `Not`, ... two-operand ops | destination | source | - |

/// Version of the instruction set, builtin ids, fuel table and memory layout.
pub const BYTECODE_VERSION: u32 = 1;

macro_rules! numbered {
    ($(#[$m:meta])* $name:ident, $repr:ty, $from:ident, [$($v:ident),* $(,)?]) => {
        $(#[$m])*
        #[repr($repr)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name { $($v),* }

        impl $name {
            /// Every member, in number order (the number is the index).
            pub const ALL: &'static [$name] = &[$($name::$v),*];

            /// The member with this number; bytecode is untrusted input.
            pub fn $from(n: $repr) -> Option<$name> {
                Self::ALL.get(usize::from(n)).copied()
            }
        }
    };
}

numbered!(
    /// One operation, numbered from 0 in the order of design D5's table.
    Op,
    u8,
    from_u8,
    [
        Nop,
        LoadSmall,
        LoadConst,
        LoadStr,
        Copy,
        Dup,
        Move,
        Drop,
        AddI,
        SubI,
        MulI,
        DivI,
        RemI,
        NegI,
        EqI,
        NeI,
        LtI,
        LeI,
        AddF,
        SubF,
        MulF,
        DivF,
        NegF,
        EqF,
        NeF,
        LtF,
        LeF,
        IntToFloat,
        FloatToInt,
        Not,
        Eq,
        Jmp,
        JmpIf,
        JmpIfNot,
        Call,
        Ret,
        TracePush,
        NewObj,
        GetField,
        TakeField,
        SetField,
        GetTag,
        MakeUnique,
        NewList,
        ListLen,
        GetIndex,
        TakeIndex,
        SetIndex,
        NewDict,
        GetKey,
        TakeKey,
        SetKey,
        RefMake,
        RefField,
        RefIndex,
        RefKey,
        RefLoad,
        RefTake,
        RefStore,
        CallBuiltin,
        CallHost,
        Await,
        AwaitAll,
        PromiseDetach,
    ]
);

numbered!(
    /// A builtin function, numbered from 0 in the order of design D5's list.
    Builtin,
    u16,
    from_u16,
    [
        ListPush,
        ListPop,
        ListInsert,
        ListRemove,
        ListSlice,
        ListContains,
        DictGet,
        DictRemove,
        DictContains,
        DictKeys,
        DictValues,
        DictLen,
        DictNext,
        DictKeyAt,
        DictValAt,
        StrLen,
        StrConcatN,
        StrSplit,
        StrStartsWith,
        StrEndsWith,
        StrContains,
        StrTrim,
        StrUpper,
        StrLower,
        ParseInt,
        Fmt,
        AbsI,
        MinI,
        MaxI,
        AbsF,
        MinF,
        MaxF,
        Sqrt,
        Floor,
        Ceil,
        Round,
        Sin,
        Cos,
        Pow,
        Assert,
        Print,
    ]
);

/// How many argument slots a builtin reads, starting at the call's `b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    Fixed(u8),
    /// `StrConcatN`: slot `b` holds the count N (an immediate int), the N strings follow in
    /// `b + 1 ..= b + N`. The interpreter range-checks N at run time.
    Counted,
}

impl Builtin {
    /// Convention for every builtin: arguments in consecutive slots from `b`, the result in `c`.
    /// Mutating builtins (`ListPush`, `ListPop`, `ListInsert`, `ListRemove`, `DictRemove`) work on the
    /// container in the first argument slot and leave it (possibly moved) there. `Fmt`'s second argument
    /// is the type id as an immediate int; `Assert`'s is the message string.
    pub fn arity(self) -> Arity {
        use Builtin::*;
        match self {
            StrConcatN => Arity::Counted,
            ListPop | DictKeys | DictValues | DictLen | StrLen | StrTrim | StrUpper | StrLower
            | ParseInt | AbsI | AbsF | Sqrt | Floor | Ceil | Round | Sin | Cos | Print => {
                Arity::Fixed(1)
            }
            ListPush | ListRemove | ListContains | DictGet | DictRemove | DictContains
            | DictNext | DictKeyAt | DictValAt | StrSplit | StrStartsWith | StrEndsWith
            | StrContains | Fmt | MinI | MaxI | MinF | MaxF | Pow | Assert => Arity::Fixed(2),
            ListInsert | ListSlice => Arity::Fixed(3),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instr {
    pub op: Op,
    pub a: u16,
    pub b: u16,
    pub c: u16,
}

impl Instr {
    /// An instruction whose `b | c << 16` is the 32-bit operand `t`.
    pub fn with_t(op: Op, a: u16, t: u32) -> Instr {
        Instr {
            op,
            a,
            b: (t & 0xffff) as u16,
            c: (t >> 16) as u16,
        }
    }

    /// The 32-bit operand `b | c << 16`.
    pub fn t(&self) -> u32 {
        u32::from(self.b) | u32::from(self.c) << 16
    }

    /// `[op, 0, a_lo, a_hi, b_lo, b_hi, c_lo, c_hi]`.
    pub fn encode(&self) -> [u8; 8] {
        let [a0, a1] = self.a.to_le_bytes();
        let [b0, b1] = self.b.to_le_bytes();
        let [c0, c1] = self.c.to_le_bytes();
        [self.op as u8, 0, a0, a1, b0, b1, c0, c1]
    }

    /// `None` for an unknown op or a non-zero padding byte (the encoding is canonical).
    pub fn decode(w: [u8; 8]) -> Option<Instr> {
        if w[1] != 0 {
            return None;
        }
        Some(Instr {
            op: Op::from_u8(w[0])?,
            a: u16::from_le_bytes([w[2], w[3]]),
            b: u16::from_le_bytes([w[4], w[5]]),
            c: u16::from_le_bytes([w[6], w[7]]),
        })
    }
}

/// What an operand names; `validate` checks it against the program's tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// Not read (any value).
    Unused,
    /// A plain number (field index, flag, tag, small integer).
    Imm,
    /// An absolute static-region word.
    Slot,
    FnId,
    Shape,
    Type,
    Builtin,
    HostSite,
}

/// What `b | c << 16` names when an op takes a 32-bit operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wide {
    None,
    /// A pc inside the function.
    Target,
    Const,
    Str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpLayout {
    pub a: Operand,
    pub b: Operand,
    pub c: Operand,
    pub wide: Wide,
}

impl Op {
    /// The operand kinds of this op. With a `wide` other than `None`, `b` and `c` are `Unused`.
    pub fn layout(self) -> OpLayout {
        use Op::*;
        use Operand::{Builtin as Bi, FnId, HostSite, Imm, Shape, Slot, Type, Unused as U};
        let l = |a, b, c| OpLayout {
            a,
            b,
            c,
            wide: Wide::None,
        };
        let w = |a, wide| OpLayout {
            a,
            b: U,
            c: U,
            wide,
        };
        match self {
            Nop | Ret => l(U, U, U),
            LoadSmall => l(Slot, Imm, U),
            LoadConst => w(Slot, Wide::Const),
            LoadStr => w(Slot, Wide::Str),
            Copy | Dup | Move | NegI | NegF | IntToFloat | FloatToInt | Not | GetTag | ListLen
            | RefLoad | RefTake | RefStore => l(Slot, Slot, U),
            Drop | MakeUnique | PromiseDetach => l(Slot, U, U),
            AddI | SubI | MulI | DivI | RemI | EqI | NeI | LtI | LeI | AddF | SubF | MulF
            | DivF | EqF | NeF | LtF | LeF | Eq | GetIndex | TakeIndex | SetIndex | GetKey
            | TakeKey | SetKey | RefIndex | RefKey => l(Slot, Slot, Slot),
            Jmp => w(U, Wide::Target),
            JmpIf | JmpIfNot => w(Slot, Wide::Target),
            Call => l(FnId, U, U),
            TracePush => l(Imm, U, U),
            NewObj => l(Slot, Shape, Imm),
            GetField | TakeField | RefField => l(Slot, Slot, Imm),
            SetField => l(Slot, Imm, Slot),
            NewList | NewDict => l(Slot, Imm, U),
            RefMake => l(Slot, Slot, Imm),
            CallBuiltin => l(Bi, Slot, Slot),
            CallHost => l(HostSite, Slot, Slot),
            Await | AwaitAll => l(Slot, Slot, Type),
        }
    }
}
