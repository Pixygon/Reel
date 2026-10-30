import fs from 'fs';
function sub(s,a,b){ if(!s.includes(a)) throw new Error('missing: '+a); return s.replace(a,b); }
let f='src/engine/picture_tests.rs', s=fs.readFileSync(f,'utf8');
s=sub(s,`    // A still with real alpha: premultiplied by the compositor on the way in.
    let mut rgba`,`    // A still with real alpha: premultiplied by the compositor on the way in.
    // No clip grade here: video.wgsl grades a still after premultiplying, so
    // semi-transparent pixels under a clip grade differ with or without masks.
    let clip = Effects::default();
    let mut rgba`);
fs.writeFileSync(f,s);
