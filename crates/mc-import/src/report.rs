//! What a conversion kept, changed and lost.

use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Cells of the trimmed box.
    pub cells: u64,
    /// Whole-block cells (marker solids included).
    pub blocks: u64,
    pub shaped: u64,
    pub carved: u64,
    /// Air-like cells left as "keep" (outside air, dropped blocks, markers without a solid, keep blocks).
    pub kept_air: u64,
    pub boxes: usize,
    /// Minecraft name -> cells placed as the fallback block.
    pub unmapped: BTreeMap<String, u64>,
    /// Reason -> cells turned into air.
    pub dropped: BTreeMap<String, u64>,
    /// Marker kind -> count.
    pub markers: BTreeMap<String, u64>,
}

impl Report {
    pub fn to_text(&self, name: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{name}: {} cells, {} blocks in {} boxes, {} shaped, {} carved, {} kept air",
            self.cells, self.blocks, self.boxes, self.shaped, self.carved, self.kept_air);
        let section = |out: &mut String, title: &str, map: &BTreeMap<String, u64>| {
            if map.is_empty() {
                return;
            }
            let _ = writeln!(out, "{title}:");
            for (k, n) in map {
                let _ = writeln!(out, "  {k}: {n}");
            }
        };
        section(&mut out, "markers", &self.markers);
        section(&mut out, "unmapped (placed as the fallback block)", &self.unmapped);
        section(&mut out, "dropped (became air)", &self.dropped);
        out
    }
}
