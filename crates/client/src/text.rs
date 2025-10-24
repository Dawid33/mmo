// let s = String::from("Hello, World!");
// for line in l.lines() {
//     for item in line.items() {
//         let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
//             continue;
//         };
//         let mut x = glyph_run.offset();
//         let y = glyph_run.baseline();
//         let run = glyph_run.run();
//         let font = run.font();
//         let font_size = run.font_size();
//         let synthesis = run.synthesis();
//         let glyph_xform = synthesis
//             .skew()
//             .map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
//         scene
//             .draw_glyphs(font)
//             // .brush(&style.brush)
//             .hint(true)
//             // .transform(transform)
//             .glyph_transform(glyph_xform)
//             .font_size(font_size)
//             .normalized_coords(run.normalized_coords())
//             .draw(
//                 Fill::NonZero,
//                 glyph_run.glyphs().map(|glyph| {
//                     let gx = x + glyph.x;
//                     let gy = y - glyph.y;
//                     x += glyph.advance;
//                     vello::Glyph {
//                         id: glyph.id as _,
//                         x: gx,
//                         y: gy,
//                     }
//                 }),
//             );
//     }
// }
