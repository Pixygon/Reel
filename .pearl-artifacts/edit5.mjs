import fs from 'fs';
function sub(s,a,b){ if(!s.includes(a)) throw new Error('missing: '+a); return s.replace(a,b); }
let f='src/masks.rs', s=fs.readFileSync(f,'utf8');
s=sub(s,`    #[test]
    fn base64_round_trips_every_tail_length() {`,`    #[test]
    fn several_masks_come_back_when_the_project_reopens() {
        let path = std::env::temp_dir().join(format!("reel-masks-{}.reel", std::process::id()));
        let path = path.to_str().unwrap();
        let mut project = crate::edit::Project::default();
        project.append_video("photo", "photo.png", 5.0);
        let sky = Plane::ellipse(64, 48, [4.0, 4.0], [60.0, 44.0]);
        let ground = Plane::rect(64, 48, [0.0, 30.0], [64.0, 48.0]);
        let clip = &mut project.tracks[0].clips[0];
        clip.masks.push(LocalMask::new("Sky", sky.clone()).unwrap());
        clip.masks.push(LocalMask::new("Ground", ground.clone()).unwrap());
        clip.masks[1].visible = false;
        clip.masks[1].adjust.saturation = 0.5;
        project.save(path).unwrap();

        let back = crate::edit::Project::load(path).unwrap();
        let _ = std::fs::remove_file(path);
        let masks = &back.tracks[0].clips[0].masks;
        assert_eq!(masks.len(), 2);
        assert_eq!((masks[0].name.as_str(), masks[0].visible), ("Sky", true));
        assert_eq!((masks[1].name.as_str(), masks[1].visible), ("Ground", false));
        assert_eq!(masks[1].adjust.saturation, 0.5);
        assert_eq!(*masks[0].matte.plane, sky);
        assert_eq!(*masks[1].matte.plane, ground);
    }

    #[test]
    fn base64_round_trips_every_tail_length() {`);
fs.writeFileSync(f,s);
f='CLAUDE.md'; s=fs.readFileSync(f,'utf8');
s=sub(s,`The graph fallback warns and drops masks; a crossfade's incoming clip and PiP previews are unmasked.`,`While a mask is active the panel hides what a mask ignores (fades, reframe, keyframes). A transition's incoming clip runs its own chain over the pool's frame (\`sync_incoming_local\`). The graph fallback warns and drops masks; overlay clips carry none.`);
s=sub(s,`  there on Mesa's software GL with a temporary \`Backends::from_env()\` and
  \`WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless\`.`,`  there on Mesa's software GL: \`Compositor\` honours
  \`WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless\`.`);
s=sub(s,`- A GPU-less box can run the compositor tests on Mesa: \`WGPU_BACKEND=gl LIBGL_ALWAYS_SOFTWARE=1 EGL_PLATFORM=surfaceless\`. Without it those tests print "no GPU" and pass without proving anything.\n\n`,'');
fs.writeFileSync(f,s);
f='docs/CLI.md'; s=fs.readFileSync(f,'utf8');
s=sub(s,`The other Adjust settings inside a mask are set in the app's Masks
panel.`,`The other colour settings inside a mask (levels, white balance, qualifier,
curves, LUT, power window, plugin) are set in the app's Masks panel.`);
fs.writeFileSync(f,s);
