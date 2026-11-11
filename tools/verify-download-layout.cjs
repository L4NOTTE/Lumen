const fs=require('node:fs'),path=require('node:path');
(async()=>{
 const dir=process.argv[2],port=Number(fs.readFileSync(path.join(dir,'profile','lumen-debug-port'),'utf8'));
 const targets=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
 for(const target of targets.filter(t=>t.type==='page')){
  const ws=new WebSocket(target.webSocketDebuggerUrl);await new Promise((resolve,reject)=>{ws.onopen=resolve;ws.onerror=reject});
  let next=0;const pending=new Map();ws.onmessage=e=>{const r=JSON.parse(e.data);if(r.id){const p=pending.get(r.id);pending.delete(r.id);if(r.error)p.reject(Error(JSON.stringify(r.error)));else p.resolve(r.result)}};
  const call=(method,params={})=>new Promise((resolve,reject)=>{const id=++next;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method,params}))});
  try{
   const ui=await call('Runtime.evaluate',{expression:"!!document.getElementById('downloadsbtn')",returnByValue:true});if(!ui.result.value)continue;
   for(const width of [800,360]){
    await call('Emulation.setDeviceMetricsOverride',{width,height:720,deviceScaleFactor:1,mobile:false});
    await call('Runtime.evaluate',{expression:'new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))',awaitPromise:true});
    const r=await call('Runtime.evaluate',{expression:"(()=>{const p=document.getElementById('p-downloads');return {width:innerWidth,overflow:p.scrollWidth>p.clientWidth+1,rows:document.querySelectorAll('.dl-row').length,badButtons:[...p.querySelectorAll('button')].filter(b=>!b.hidden&&b.getBoundingClientRect().right>innerWidth).length}})()",returnByValue:true});
    const l=r.result.value;if(l.width!==width||l.overflow||l.badButtons||l.rows!==2)throw Error('Download layout failed: '+JSON.stringify(l));
    const shot=await call('Page.captureScreenshot',{format:'png',clip:{x:0,y:0,width,height:720,scale:1}});
    fs.writeFileSync(path.join(dir,`downloads-${width}.png`),Buffer.from(shot.data,'base64'));
    console.log('Download layout: '+JSON.stringify(l));
   }
   await call('Emulation.clearDeviceMetricsOverride');return;
  }finally{ws.close()}
 }
 throw Error('Browser UI missing');
})().catch(e=>{console.error(e);process.exitCode=1});
