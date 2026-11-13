const fs = require('node:fs'), vm = require('node:vm'), assert = require('node:assert/strict');
const [baseline, current, output] = process.argv.slice(2);
function previewFrom(file) {
  const source = fs.readFileSync(file, 'utf8');
  for (const [, script] of source.matchAll(/<script>([\s\S]*?)<\/script>/g)) new vm.Script(script);
  const preview = source.match(/    const preview = \(\) => \{[\s\S]*?(?=\n    const move = ev =>)/)?.[0];
  if (!preview) throw Error('Drag preview missing in ' + file);
  const search = source.match(/function dragSlot\(centers, x\) \{[\s\S]*?\n\}/)?.[0];
  return new vm.Script('(() => {\n' + (search || '') + '\n' + preview + '\nreturn preview;\n})()');
}
function sample(script, count, modern) {
  let writes = 0;
  const tabs = Array.from({length:count}, () => {
    let transform = '';
    return {
      style: {get transform(){return transform;},set transform(value){transform=value; writes++;}},
      classList: {add(){},toggle(){}},
    };
  });
  const startIndex = Math.floor(count/2), d = tabs[startIndex], slots = tabs.map((_, i) => ({left:i*30,width:28}));
  const otherTabs = tabs.filter(el => el !== d);
  const others = modern ? tabs.flatMap((el,slot)=>el===d?[]:[{el,slot}]) : otherTabs;
  const centers = slots.filter((_,i)=>i!==startIndex).map(s=>s.left+s.width/2);
  const sx=slots[startIndex].left+14;
  const context=vm.createContext({tabs,slots,startIndex,d,others,centers,sx,sy:20,px:sx,py:20,index:startIndex,moved:false,detached:false,canDetach:true,frame:0,innerWidth:slots.at(-1).left+30});
  const run=script.runInContext(context);
  // Exercise both directions, reversals and crossing the detach threshold.
  for(const [offset,py] of [[95,20],[-97,20],[87,90],[-80,20],[0,20]]) {
    context.px=sx+offset; context.py=py; run();
    if(!modern) continue;
    const target=Math.abs(py-20)>46 ? startIndex : centers.filter(x=>x<sx+offset).length;
    assert.equal(context.index,target);
    others.forEach(({el,slot},i)=>{
      const delta=slots[i<target?i:i+1].left-slots[slot].left;
      assert.equal(el.style.transform,delta ? 'translateX('+delta+'px)' : '');
    });
  }
  writes=0;
  for(let frame=0;frame<1200;frame++){
    context.px=sx+Math.sin(frame/60)*(startIndex*30-40);
    context.py=20; run();
  }
  return writes;
}
const before=previewFrom(baseline), after=previewFrom(current);
const rows=[24,60].map(tabs=>({tabs,frames:1200,before:sample(before,tabs,false),after:sample(after,tabs,true)}));
assert(rows.every(row=>row.after<row.before));
const report={metric:'Assignments to style.transform in the actual drag preview code, with a mock DOM. This is not a frame rate measurement.',rows};
fs.writeFileSync(output,JSON.stringify(report,null,2));
console.log(report);
