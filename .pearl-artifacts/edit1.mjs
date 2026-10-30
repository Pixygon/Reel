import fs from 'fs';
function sub(s,a,b){ if(!s.includes(a)) throw new Error('missing: '+a); return s.replace(a,b); }
// ---- ui.rs
let f='src/ui.rs', s=fs.readFileSync(f,'utf8');
s=sub(s,`                mask_panel(ui, app, id);
                ui.add_space(6.0);`,`                mask_panel(ui, app, id);
                // A mask carries colour only: what it would ignore isn't shown.
                let in_mask = app.project.clip(id).is_some_and(|c| c.active_mask.is_some());
                ui.add_space(6.0);`);
s=sub(s,`                ui.add(egui::Slider::new(&mut fx.fade_in, 0.0..=duration.min(5.0)).text("Fade in (s)"));
                ui.add(egui::Slider::new(&mut fx.fade_out, 0.0..=duration.min(5.0)).text("Fade out (s)"));
                // Reframe — how you put a landscape shot into a vertical
                // frame without blurred sides.
                ui.horizontal(|ui| {`,`                if !in_mask {
                ui.add(egui::Slider::new(&mut fx.fade_in, 0.0..=duration.min(5.0)).text("Fade in (s)"));
                ui.add(egui::Slider::new(&mut fx.fade_out, 0.0..=duration.min(5.0)).text("Fade out (s)"));
                // Reframe — how you put a landscape shot into a vertical
                // frame without blurred sides.
                ui.horizontal(|ui| {`);
s=sub(s,`                    ui.add(egui::Slider::new(&mut fx.pan_y, -1.0..=1.0).text("Pan ↕"));
                }
`,`                    ui.add(egui::Slider::new(&mut fx.pan_y, -1.0..=1.0).text("Pan ↕"));
                }
                }
`);
s=sub(s,`                // ── Keyframes ───────────────────────────────────────────
`,`                if !in_mask {
                // ── Keyframes ───────────────────────────────────────────
`);
s=sub(s,`                // ── LUT ─────────────────────────────────────────────────
`,`                }

                // ── LUT ─────────────────────────────────────────────────
`);
s=sub(s,`"The sliders below now edit this mask: colour only, inside the selection."`,`"Look, curves, LUT, power window and plugin now edit this mask, inside the selection. Everything else stays the clip's."`);
s=sub(s,`            ui.add(egui::Slider::new(&mut sel.brush_radius, 1.0..=400.0).logarithmic(true).text("Size"));
`,`            ui.add(egui::Slider::new(&mut sel.brush_radius, 1.0..=400.0).logarithmic(true).text("Size"));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut sel.brush_remove, false, "Add");
                ui.selectable_value(&mut sel.brush_remove, true, "Remove");
            });
`);
s=sub(s,`        let op = combine_for(shift, alt, tool);
        let sel = &mut app.selection;`,`        let sel = &mut app.selection;
        let op = match combine_for(shift, alt, tool) {
            crate::masks::Combine::Add if tool == Tool::Brush && !shift && sel.brush_remove => {
                crate::masks::Combine::Subtract
            }
            op => op,
        };`);
s=sub(s,`fn drag_title(app: &mut ReelApp, response: &egui::Response, pic: Rect) {
    if app.mode != Mode::Editor || pic.width() <= 0.0 {`,`fn drag_title(app: &mut ReelApp, response: &egui::Response, pic: Rect) {
    if app.mode != Mode::Editor || app.selection.tool.is_some() || pic.width() <= 0.0 {`);
s=sub(s,`                                crate::video_pass::VideoDraw {
                                    view: tex
                                        .texture
                                        .create_view(&wgpu::TextureViewDescriptor::default()),`,`                                crate::video_pass::VideoDraw {
                                    view: app.local_incoming_view().unwrap_or_else(|| {
                                        tex.texture.create_view(&wgpu::TextureViewDescriptor::default())
                                    }),`);
fs.writeFileSync(f,s);
// ---- masks.rs
f='src/masks.rs'; s=fs.readFileSync(f,'utf8');
s=sub(s,`    pub brush_radius: f32,
    pub tolerance: f32,`,`    pub brush_radius: f32,
    /// The brush takes away instead of painting. Alt does the same, but a
    /// window manager may own Alt+drag.
    pub brush_remove: bool,
    pub tolerance: f32,`);
s=sub(s,`            brush_radius: 40.0,
`,`            brush_radius: 40.0,
            brush_remove: false,
`);
fs.writeFileSync(f,s);
// ---- export.rs: the doc comment stays on chain
f='src/export.rs'; s=fs.readFileSync(f,'utf8');
const doc=`    /// The filter chain that maps any input to exactly \`w\`×\`h\`.
    /// \`tag\` keeps labels unique when several of these appear in one graph.
`;
s=sub(s,doc,'');
s=sub(s,`    pub(crate) fn chain(self, w: u32, h: u32, tag: &str) -> String {`,doc+`    pub(crate) fn chain(self, w: u32, h: u32, tag: &str) -> String {`);
fs.writeFileSync(f,s);
// ---- app.rs: the incoming half of a transition
f='src/app.rs'; s=fs.readFileSync(f,'utf8');
s=sub(s,`    comp: crate::engine::compositor::Compositor,
    built: Option<LocalBuilt>,
}`,`    comp: crate::engine::compositor::Compositor,
    built: Option<LocalBuilt>,
    /// The incoming clip of a transition: a second picture, a second chain.
    incoming: Option<LocalBuilt>,
}`);
s=sub(s,`    /// Run the local-mask chain over the frame just synced.
    pub fn sync_local_preview(&mut self, gpu: &Gpu) {
        let Some(clip) = self.masked_clip()`,`    fn local_preview(&mut self, gpu: &Gpu) -> &mut LocalPreview {
        self.local.get_or_insert_with(|| LocalPreview {
            comp: crate::engine::compositor::Compositor::with_device(
                gpu.device.clone(),
                gpu.queue.clone(),
            ),
            built: None,
            incoming: None,
        })
    }

    /// The incoming half of a transition, over the preview pool's frame. The
    /// pool doesn't rotate its players, so that frame is the stored picture.
    fn sync_incoming_local(&mut self, gpu: &Gpu) {
        let masked = self.transition_preview.and_then(|(id, ..)| {
            let masks = self.project.clip(id)?.masks.clone();
            let tex = self.overlay_previews.get(&id)?.tex.as_ref()?;
            let view = tex.texture.create_view(&wgpu::TextureViewDescriptor::default());
            masks.iter().any(|m| m.visible).then_some((masks, view, tex.width, tex.height))
        });
        let Some((masks, frame, w, h)) = masked else {
            if let Some(l) = &mut self.local {
                l.incoming = None;
            }
            return;
        };
        let key: LocalKey =
            (masks.iter().map(|m| (m.matte.uid, m.visible, m.adjust)).collect(), w, h, 0);
        let (luts, plugins) = (self.project.luts.clone(), self.project.plugins.clone());
        let local = self.local_preview(gpu);
        if local.incoming.as_ref().is_none_or(|b| b.key != key) {
            local.incoming = crate::engine::render::local_chain(
                &local.comp,
                &masks,
                (w, h),
                |_| [0.0, 0.0, 1.0, 1.0],
                0,
                false,
                &luts,
                &plugins,
            )
            .map(|chain| LocalBuilt { key, chain, still: None, view: None });
        }
        if let Some(b) = &mut local.incoming {
            b.view = Some(b.chain.run(&local.comp, &frame));
        }
    }

    /// The incoming clip's frame with its masks, while a transition shows it.
    pub fn local_incoming_view(&self) -> Option<wgpu::TextureView> {
        self.local.as_ref()?.incoming.as_ref()?.view.clone()
    }

    /// Run the local-mask chain over the frame just synced.
    pub fn sync_local_preview(&mut self, gpu: &Gpu) {
        self.sync_incoming_local(gpu);
        let Some(clip) = self.masked_clip()`);
s=sub(s,`        let masks = clip.masks.clone();
        let local = self.local.get_or_insert_with(|| LocalPreview {
            comp: crate::engine::compositor::Compositor::with_device(
                gpu.device.clone(),
                gpu.queue.clone(),
            ),
            built: None,
        });
        if local.built`,`        let masks = clip.masks.clone();
        let local = self.local_preview(gpu);
        if local.built`);
fs.writeFileSync(f,s);
// docs
f='README.md'; s=fs.readFileSync(f,'utf8');
fs.writeFileSync(f,s);
