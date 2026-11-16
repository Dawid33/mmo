//! The state hash, saving and loading an instance mid-execution (spec §6.4, §6.8).
//!
//! The state of an instance is its program's identity and its flat memory; the undo log is not part
//! of it (design D3). A save, all little-endian:
//!
//! | bytes | field |
//! |---|---|
//! | 8 | magic `QSSAVE01` |
//! | 4 | [`BYTECODE_VERSION`] |
//! | 4 | the program's host-API version |
//! | 8 | the program's content hash |
//! | 4 | the memory's length `n` |
//! | `n` | the memory |
//! | 4 | the orphan count |
//! | … | each orphan (`migrate`): its name, its type text, its value's wire bytes, each a `u32` byte length and the bytes |
//!
//! The orphans are in their canonical order (sorted by name, then type text, no pair twice): a save
//! that breaks it is refused, so equal states have equal saves and hashes.
//!
//! [`Vm::load`] checks the envelope and the memory's header (versions, program hash, length, every
//! region base); it does not parse the scheduler's or the heap's contents. A loaded memory that is
//! malformed there fails at run time like any corrupt memory (`FaultKind::Corrupt`, `Status::Failed`).

use std::hash::Hasher;
use std::sync::Arc;

use state_hash::StateHash;

use crate::bytecode::BYTECODE_VERSION;
use crate::hash::Fnv64;
use crate::layout::{
    H_BYTECODE_VERSION, H_HOST_API_VERSION, H_PROGRAM_HASH, H_STATIC_WORDS, Layout, LoadError,
};
use crate::mem::Mem;
use crate::migrate::Orphan;
use crate::program::Program;
use crate::store::Store;
use crate::vm::{Vm, VmConfig, check_program};
use crate::wire::WireValue;

/// The first bytes of every save.
pub const SAVE_MAGIC: [u8; 8] = *b"QSSAVE01";

impl StateHash for Vm {
    /// The bytecode version, the program's content hash, the memory, the orphans (count, then each one's
    /// name, type text and wire bytes) — never the undo log, never the configuration.
    fn state_hash<H: Hasher>(&self, h: &mut H) {
        BYTECODE_VERSION.state_hash(h);
        self.program_hash.state_hash(h);
        self.store.mem.state_hash(h);
        (self.orphans.len() as u64).state_hash(h);
        for o in &self.orphans {
            o.name.state_hash(h);
            o.ty.state_hash(h);
            let mut bytes = Vec::new();
            o.value.encode(&mut bytes);
            bytes.state_hash(h);
        }
    }
}

/// A cursor over a save; running past the end is `Invalid`.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        if self.rest.len() < n {
            return Err(LoadError::Invalid("the save is truncated".into()));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        let mut b = [0; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b))
    }

    /// A `u32` byte length and that many bytes.
    fn bytes(&mut self) -> Result<&'a [u8], LoadError> {
        let n = self.u32()?;
        self.take(n as usize)
    }

    fn text(&mut self) -> Result<String, LoadError> {
        String::from_utf8(self.bytes()?.to_vec())
            .map_err(|_| LoadError::Invalid("an orphan's text is not UTF-8".into()))
    }

    fn u64(&mut self) -> Result<u64, LoadError> {
        let mut b = [0; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }
}

impl Vm {
    /// FNV-1a 64 over [`StateHash`]: equal on every platform for equal states.
    pub fn hash64(&self) -> u64 {
        let mut h = Fnv64::default();
        self.state_hash(&mut h);
        h.finish()
    }

    /// The complete state, mid-execution or not (format in the module docs).
    pub fn save(&self) -> Vec<u8> {
        let mem = self.store.mem.bytes();
        let mut out = Vec::with_capacity(mem.len().saturating_add(32));
        out.extend_from_slice(&SAVE_MAGIC);
        out.extend_from_slice(&BYTECODE_VERSION.to_le_bytes());
        out.extend_from_slice(&self.program.host_api_version.to_le_bytes());
        out.extend_from_slice(&self.program_hash.to_le_bytes());
        out.extend_from_slice(&self.store.mem.len().to_le_bytes());
        out.extend_from_slice(mem);
        let put = |out: &mut Vec<u8>, b: &[u8]| {
            // An orphan's parts came out of a memory of at most 4 GiB.
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        };
        out.extend_from_slice(&(self.orphans.len() as u32).to_le_bytes());
        for o in &self.orphans {
            put(&mut out, o.name.as_bytes());
            put(&mut out, o.ty.as_bytes());
            let mut value = Vec::new();
            o.value.encode(&mut value);
            put(&mut out, &value);
        }
        out
    }

    /// The instance saved in `bytes`, resumed exactly where it was, with an empty undo log. `program`
    /// must be the one it was saved with (`ProgramMismatch`) and `cfg.host_api_version` the program's;
    /// `cfg.events` configures it as in [`Vm::new`]. `cfg.mem_bytes` and `cfg.epoch` are not used: the
    /// memory carries both.
    pub fn load(program: Arc<Program>, cfg: VmConfig, bytes: &[u8]) -> Result<Vm, LoadError> {
        let mut r = Reader { rest: bytes };
        if r.take(SAVE_MAGIC.len()).ok() != Some(&SAVE_MAGIC[..]) {
            return Err(LoadError::BadMagic);
        }
        let bytecode_version = r.u32()?;
        if bytecode_version != BYTECODE_VERSION {
            return Err(LoadError::VersionMismatch {
                found: bytecode_version,
            });
        }
        let host_api_version = r.u32()?;
        if host_api_version != program.host_api_version {
            return Err(LoadError::HostApiMismatch {
                found: host_api_version,
            });
        }
        check_program(&program, cfg.host_api_version)?;
        let program_hash = program.content_hash();
        if r.u64()? != program_hash {
            return Err(LoadError::ProgramMismatch);
        }
        let mem_len = r.u32()?;
        let mem = r.take(mem_len as usize)?;
        let mut orphans: Vec<Orphan> = Vec::new();
        for _ in 0..r.u32()? {
            let (name, ty) = (r.text()?, r.text()?);
            let value = WireValue::decode(r.bytes()?)
                .map_err(|e| LoadError::Invalid(format!("an orphan's value: {e:?}")))?;
            if orphans
                .last()
                .is_some_and(|l| (&l.name, &l.ty) >= (&name, &ty))
            {
                return Err(LoadError::Invalid("the orphans are out of order".into()));
            }
            orphans.push(Orphan { name, ty, value });
        }
        if !r.rest.is_empty() {
            return Err(LoadError::Invalid("bytes after the save".into()));
        }
        if !mem_len.is_multiple_of(8) {
            return Err(LoadError::Invalid(
                "the memory is not a whole number of words".into(),
            ));
        }
        let mem = Mem::from_bytes(mem.to_vec());
        // The magic, `mem_bytes` against the memory's length, and every region base against the plan
        // for the header's static words — which must be the program's.
        let layout = Layout::read_header(&mem)?;
        let header = |w: u32| mem.r64(w * 8).ok();
        let expected = [
            (H_STATIC_WORDS, u64::from(program.static_words)),
            (H_BYTECODE_VERSION, u64::from(BYTECODE_VERSION)),
            (H_HOST_API_VERSION, u64::from(program.host_api_version)),
            (H_PROGRAM_HASH, program_hash),
        ];
        for (w, want) in expected {
            if header(w) != Some(want) {
                return Err(LoadError::Invalid(format!(
                    "header word {w} disagrees with the program"
                )));
            }
        }
        let store = Store { mem, layout };
        let mut vm = Vm::adopt(program, program_hash, store, cfg.events);
        vm.orphans = orphans;
        Ok(vm)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::Op;
    use crate::asm::ProgramBuilder;
    use crate::vm::{Vm, VmConfig};

    /// `main` loads constant `x`.
    fn program(x: u64) -> Arc<crate::program::Program> {
        let mut pb = ProgramBuilder::new(1);
        let c = pb.const_u64(x);
        let mut f = pb.func("main", 0);
        let ret = f.ret();
        f.emit_t(Op::LoadConst, ret, c);
        f.emit(Op::Ret, 0u16, 0u16, 0u16);
        f.end();
        Arc::new(pb.finish().expect("program"))
    }

    #[test]
    fn hash_covers_program_identity() {
        let cfg = VmConfig {
            mem_bytes: 65_536,
            epoch: 1,
            host_api_version: 1,
            events: vec![],
        };
        let a = Vm::new(program(1), cfg.clone()).expect("a");
        let mut b = Vm::new(program(2), cfg).expect("b");
        // The header carries the program hash too: give `b` exactly `a`'s memory.
        b.store = a.store.clone();
        assert_eq!(a.mem_bytes(), b.mem_bytes());
        assert_ne!(a.hash64(), b.hash64());
    }
}
