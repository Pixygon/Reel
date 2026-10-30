import fs from 'fs';
// compose.wgsl: keep both
let f='src/engine/compose.wgsl', s=fs.readFileSync(f,'utf8');
s=s.replace(/<<<<<<< ours\n([\s\S]*?)\|\|\|\|\|\|\| base\n=======\n([\s\S]*?)>>>>>>> theirs\n/g,'$1$2');
fs.writeFileSync(f,s);
f='src/engine/compositor.rs'; s=fs.readFileSync(f,'utf8');
let n=0;
s=s.replace(/<<<<<<< ours\n([\s\S]*?)\|\|\|\|\|\|\| base\n([\s\S]*?)=======\n([\s\S]*?)>>>>>>> theirs\n/g,(m,o,b,t)=>{
 n++;
 if(n==1) return o+t;
 if(n==2) return t+o;
 if(n==3) return `                    contents: bytemuck::bytes_of(&Self::uniforms(l, frame_aspect, matte)),\n`;
 if(n==4) return o.replace('fn uniforms(l: &Layer, frame_aspect: f32)','fn uniforms(l: &Layer, frame_aspect: f32, matte: Option<&MatteBind>)');
 if(n==5) return o+t;
});
fs.writeFileSync(f,s);
f='CHANGELOG.md'; s=fs.readFileSync(f,'utf8');
s=s.replace(/<<<<<<< ours\n([\s\S]*?)\|\|\|\|\|\|\| base\n=======\n([\s\S]*?)>>>>>>> theirs\n/,(m,o,t)=>{
 const ent=t.replace('## Unreleased\n\n','').trimEnd();
 return o.replace('### Added\n','### Added\n'+ent+'\n');
});
fs.writeFileSync(f,s);
