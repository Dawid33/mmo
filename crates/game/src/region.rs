use std::collections::BTreeMap;

use parley::{Alignment, AlignmentOptions, FontContext, Layout, LayoutContext, StyleProperty};

use crate::{EntityId, EntityType, GameData};

pub fn text_layout<'a>(fctx: &mut FontContext, text: &str) -> Layout<()> {
    let mut l: LayoutContext<()> = LayoutContext::new();
    let mut l = l.ranged_builder(fctx, &text, 1.0, true);
    l.push_default(StyleProperty::FontSize(16.0));
    let mut layout = l.build(text);
    layout.align(None, Alignment::Start, AlignmentOptions::default());
    layout.break_all_lines(Some(10000.0));
    layout
}

pub struct RegionData {
    pub data: GameData,
    pub font_context: FontContext,
    pub text_layouts: BTreeMap<EntityId, Layout<()>>,
}

impl RegionData {
    pub fn new(data: GameData, mut font_context: FontContext) -> Self {
        let mut text_layouts = BTreeMap::new();
        for (i, e) in data.raw().entities.iter().enumerate() {
            match &e.kind {
                EntityType::Text { content } => {
                    let l = text_layout(&mut font_context, &content);
                    text_layouts.insert(i, l);
                }

                _ => (),
            }
        }
        Self {
            text_layouts,
            font_context,
            data,
        }
    }
}
