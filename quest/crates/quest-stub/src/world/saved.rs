//! What a [`World`]'s saved state keeps beside its instances' VMs ([`World::saved`], [`World::restore`];
//! `quest run --save` / `--load` write and read it): the `Setup` the world has due, what each part was last
//! sent, and each instance's last dialogue mirror — state kept as it is, never re-derived at a load. Not kept:
//! the clock (a loaded world starts at tick 0), the people (from `people.txt`), the namespace, the
//! conversations and their records, the scenes, the attachments, and the events and mirrors on their way.

use std::collections::BTreeMap;

use quest_vm::Mirror;
use quest_vm::wire::WireValue;

use super::World;

/// The world-side saved state ([module docs](self)).
#[derive(Clone, Debug, PartialEq)]
pub struct WorldSaved {
    /// Ticks from the save until the `Setup` the world had due, if one was.
    pub setup_in: Option<u64>,
    /// The hash of the mirror last sent to each part.
    pub sent: BTreeMap<u16, u64>,
    /// Each instance's last dialogue mirror, by instance id.
    pub mirrors: BTreeMap<u32, Mirror>,
}

/// `QSWORLD1`, then the fields in order: a `u8` flag and `u64` for `setup_in`; a `u32` count of `(u16, u64)`
/// for `sent`; a `u32` count of mirrors, each a `u32` instance, the `u64` program hash, a `u32` count of
/// globals, each a `u16` and a `u32`-length-prefixed wire value. Little-endian.
const MAGIC: &[u8; 8] = b"QSWORLD1";

impl WorldSaved {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        match self.setup_in {
            Some(t) => {
                out.push(1);
                out.extend_from_slice(&t.to_le_bytes());
            }
            None => out.push(0),
        }
        out.extend_from_slice(&len32(self.sent.len()).to_le_bytes());
        for (id, h) in &self.sent {
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&h.to_le_bytes());
        }
        out.extend_from_slice(&len32(self.mirrors.len()).to_le_bytes());
        for (i, m) in &self.mirrors {
            out.extend_from_slice(&i.to_le_bytes());
            out.extend_from_slice(&m.program.to_le_bytes());
            out.extend_from_slice(&len32(m.globals.len()).to_le_bytes());
            for (g, v) in &m.globals {
                out.extend_from_slice(&g.to_le_bytes());
                let mut bytes = Vec::new();
                v.encode(&mut bytes);
                out.extend_from_slice(&len32(bytes.len()).to_le_bytes());
                out.extend_from_slice(&bytes);
            }
        }
        out
    }

    /// `Err` on any bytes [`WorldSaved::to_bytes`] did not write (never a panic; no count trusted before
    /// the bytes it needs are there).
    pub fn from_bytes(bytes: &[u8]) -> Result<WorldSaved, String> {
        let mut r = Reader(bytes.strip_prefix(&MAGIC[..]).ok_or("not a world state")?);
        let setup_in = match r.u8()? {
            0 => None,
            1 => Some(r.u64()?),
            _ => return Err("bad setup flag".into()),
        };
        let mut sent = BTreeMap::new();
        for _ in 0..r.u32()? {
            let id = r.u16()?;
            sent.insert(id, r.u64()?);
        }
        let mut mirrors = BTreeMap::new();
        for _ in 0..r.u32()? {
            let i = r.u32()?;
            let program = r.u64()?;
            let mut globals = BTreeMap::new();
            for _ in 0..r.u32()? {
                let g = r.u16()?;
                let len = r.u32()?;
                let v = WireValue::decode(r.take(len)?).map_err(|e| format!("{e:?}"))?;
                globals.insert(g, v);
            }
            mirrors.insert(i, Mirror { program, globals });
        }
        if !r.0.is_empty() {
            return Err("unexpected bytes after the world state".into());
        }
        Ok(WorldSaved {
            setup_in,
            sent,
            mirrors,
        })
    }
}

/// A count below 4 Gi (the stub's maps are far smaller).
fn len32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: u32) -> Result<&'a [u8], String> {
        let n = usize::try_from(n).map_err(|_| "truncated")?;
        if self.0.len() < n {
            return Err("truncated".into());
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, String> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(a))
    }
}

impl World {
    /// The world-side saved state, to be kept beside each instance's `Vm::save` ([module docs](self)).
    pub fn saved(&self) -> WorldSaved {
        WorldSaved {
            setup_in: self.setup_at.map(|at| at.saturating_sub(self.core.env.now)),
            sent: self.sent.clone(),
            mirrors: self.core.env.saved_mirrors(),
        }
    }

    /// Puts back a [`World::saved`] before [`World::load_vms`] (which then keeps it, except where a loaded
    /// VM contradicts it): the `Setup` due that many ticks from now, what each part was sent, the dialogue
    /// mirrors. Pass `load_vms` [`World::setup_due`] after it.
    pub fn restore(&mut self, saved: WorldSaved) {
        self.setup_at = saved.setup_in.map(|t| self.core.env.now.saturating_add(t));
        self.sent = saved.sent;
        self.core.env.restore_mirrors(saved.mirrors);
    }
}
