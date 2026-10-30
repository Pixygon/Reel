import fs from 'fs';
function sub(s,a,b){ if(!s.includes(a)) throw new Error('missing: '+a); return s.split(a).join(b); }
let f='src/app.rs', s=fs.readFileSync(f,'utf8');
s=sub(s,`    fn local_preview(&mut self, gpu: &Gpu) -> &mut LocalPreview {
        self.local.get_or_insert_with(|| LocalPreview {`,`    fn local_preview<'a>(slot: &'a mut Option<LocalPreview>, gpu: &Gpu) -> &'a mut LocalPreview {
        slot.get_or_insert_with(|| LocalPreview {`);
s=sub(s,`        let (luts, plugins) = (self.project.luts.clone(), self.project.plugins.clone());
        let local = self.local_preview(gpu);`,`        let local = Self::local_preview(&mut self.local, gpu);`);
s=sub(s,`                &luts,
                &plugins,
            )
            .map(|chain|`,`                &self.project.luts,
                &self.project.plugins,
            )
            .map(|chain|`);
s=sub(s,`        let local = self.local_preview(gpu);`,`        let local = Self::local_preview(&mut self.local, gpu);`);
fs.writeFileSync(f,s);
f='src/ui.rs'; s=fs.readFileSync(f,'utf8');
s=sub(s,`        let op = match combine_for(shift, alt, tool) {
            crate::masks::Combine::Add if tool == Tool::Brush && !shift && sel.brush_remove => {
                crate::masks::Combine::Subtract
            }
            op => op,
        };`,`        let op = sel.combine(shift, alt, tool);`);
s=sub(s,`    use crate::masks::{combine_for, view_to_source, Plane, Tool};`,`    use crate::masks::{view_to_source, Plane, Tool};`);
fs.writeFileSync(f,s);
f='src/masks.rs'; s=fs.readFileSync(f,'utf8');
s=sub(s,`impl Selection {
`,`impl Selection {
    /// How a gesture joins the selection: the modifiers, then the brush's
    /// Add/Remove switch.
    pub fn combine(&self, shift: bool, alt: bool, tool: Tool) -> Combine {
        match combine_for(shift, alt, tool) {
            Combine::Add if tool == Tool::Brush && !shift && self.brush_remove => Combine::Subtract,
            op => op,
        }
    }

`);
s=sub(s,`    #[test]
    fn modifiers_choose_how_a_gesture_joins() {`,`    #[test]
    fn the_brush_switch_removes_without_a_modifier() {
        let mut sel = Selection { brush_remove: true, ..Default::default() };
        sel.target(1, (100, 100));
        sel.apply(Plane::rect(100, 100, [0.0, 0.0], [100.0, 100.0]), Combine::Replace);
        let op = sel.combine(false, false, Tool::Brush);
        sel.apply(Plane::stroke(100, 100, &[[50.0, 50.0]], 10.0), op);
        let plane = sel.plane.as_ref().unwrap();
        assert_eq!(at(plane, 50, 50), 0);
        assert_eq!(at(plane, 5, 5), 255);
        assert_eq!(sel.combine(true, false, Tool::Brush), Combine::Add, "Shift still adds");
        assert_eq!(sel.combine(false, false, Tool::Lasso), Combine::Replace);
    }

    #[test]
    fn modifiers_choose_how_a_gesture_joins() {`);
fs.writeFileSync(f,s);
f='README.md'; s=fs.readFileSync(f,'utf8');
s=sub(s,`- **Power windows.** Limit any grade`,`- **Selections and local edits.** Lasso, polygon, rectangle, ellipse, brush
  and colour range, with add/subtract/intersect, feather, grow and invert.
  A selection becomes a named, hideable mask with its own grade, stored in
  the project at picture resolution — on a photo, or as a static matte on a
  video clip. The preview and the render run the same GPU chain.
- **Power windows.** Limit any grade`);
fs.writeFileSync(f,s);
