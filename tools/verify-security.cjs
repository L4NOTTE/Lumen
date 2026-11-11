const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),http=require('node:http'),{spawn}=require('node:child_process');
const sleep=n=>new Promise(r=>setTimeout(r,n));
const png='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j/XcAAAAASUVORK5CYII=';
async function connect(target){
 const ws=new WebSocket(target.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});let id=0;const pending=new Map();
 ws.onmessage=e=>{const m=JSON.parse(e.data),p=pending.get(m.id);if(p){clearTimeout(p.timer);pending.delete(m.id);m.error?p.j(Error(JSON.stringify(m.error))):p.r(m.result)}};
 return {close:()=>ws.close(),call:(method,params={})=>new Promise((r,j)=>{const n=++id,timer=setTimeout(()=>{pending.delete(n);j(Error(method+' timeout: '+String(params.expression||'').slice(0,100)))},10000);pending.set(n,{r,j,timer});ws.send(JSON.stringify({id:n,method,params}))})};
}
(async()=>{
 const [exe,dir,mode='fixed']=process.argv.slice(2),baseline=mode==='baseline';fs.mkdirSync(dir,{recursive:true});
 if(fs.existsSync(path.join(dir,'settings.json')))throw Error('Use a fresh audit profile');
 fs.writeFileSync(path.join(dir,'settings.json'),JSON.stringify({adblock:false,startup:'new'}));fs.writeFileSync(path.join(dir,'background.txt'),png);
 const children=[],connections=[],results=[];
 const server=http.createServer((req,res)=>{res.writeHead(200,{'Content-Type':'text/html'});res.end('<title>untrusted</title><script>window.ipc?.postMessage(JSON.stringify({cmd:"set_startup",mode:"restore"}))</script>')});
 await new Promise(r=>server.listen(0,'127.0.0.1',r));const attack=`http://127.0.0.1:${server.address().port}/attack`;
 const evaluate=async(c,expression)=>(await c.call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true})).result?.value;
 const send=(c,value)=>evaluate(c,`window.ipc.postMessage(${JSON.stringify(JSON.stringify(value))})`);
 async function launch(privateMode=false){
  const proc=spawn(exe,['--secondary',...(privateMode?['--incognito']:[])],{windowsHide:true,env:{...process.env,LUMEN_DATA_DIR:dir,LUMEN_TEST_HIDDEN:'1',LUMEN_ENABLE_CDP:'1'}});children.push(proc);
  let errors='';proc.stderr?.on('data',d=>errors+=d);const profile=privateMode?path.join(os.tmpdir(),`Lumen-incognito-${proc.pid}`):path.join(dir,'profile');
  for(let n=0;n<100;n++){
   await sleep(100);if(proc.exitCode!==null)throw Error('Browser exited: '+errors);
   try{const port=Number(fs.readFileSync(path.join(profile,'lumen-debug-port'),'utf8')),targets=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    for(const t of targets.filter(t=>t.type==='page')){const c=await connect(t);if(await evaluate(c,"!!document.getElementById('devbtn')&&typeof state!=='undefined'")){connections.push(c);return {proc,port,profile,ui:c};}c.close();}
   }catch{}
  }throw Error('No test UI: '+errors);
 }
 function check(name,ok,details){results.push({name,passed:!!ok,details});if(!ok)throw Error(name+': '+JSON.stringify(details));}
 try{
  const normal=await launch();
  const hasPolicy=await evaluate(normal.ui,"(()=>{const p=document.querySelector('meta[http-equiv=\"Content-Security-Policy\"]')?.content||'';return p.includes(\"'sha256-\")&&!p.includes(\"script-src 'unsafe-inline'\")})()");
  check('hashed_ui_csp',baseline?!hasPolicy:hasPolicy,{baseline,hasPolicy});
  await evaluate(normal.ui,"window.auditInjected=0;const s=document.createElement('script');s.textContent='window.auditInjected=1';document.body.append(s)");
  const injected=await evaluate(normal.ui,'window.auditInjected');check('inline_script_injection',baseline?injected===1:injected===0,{injected});
  const bgBefore=fs.readFileSync(path.join(dir,'background.txt'),'utf8');
  await send(normal.ui,{cmd:'set_background',data:'data:image/png;base64,");}</style><script>window.auditBg=1</script>'});await sleep(200);
  const bgAfter=fs.readFileSync(path.join(dir,'background.txt'),'utf8');check('background_markup_rejected',baseline?bgAfter!==bgBefore:bgAfter===bgBefore,{unchanged:bgAfter===bgBefore});
  fs.writeFileSync(path.join(dir,'background.txt'),png);
  const fakeId='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',fakeName=fakeId+'-audit-fixture',fakeDir=path.join(dir,'extensions',fakeName);
  fs.mkdirSync(fakeDir,{recursive:true});fs.writeFileSync(path.join(fakeDir,'keep.txt'),'audit fixture');fs.writeFileSync(path.join(dir,'ext_map.json'),JSON.stringify({[fakeId]:fakeName}));
  const privateWindow=await launch(true);await send(privateWindow.ui,{cmd:'clear_background'});await sleep(250);
  check('incognito_preserves_regular_background',baseline?!fs.existsSync(path.join(dir,'background.txt')):fs.existsSync(path.join(dir,'background.txt')),{exists:fs.existsSync(path.join(dir,'background.txt'))});
  if(!baseline){await send(privateWindow.ui,{cmd:'ext_remove',id:fakeId});await sleep(2300);check('incognito_cannot_delete_regular_extensions',fs.existsSync(path.join(fakeDir,'keep.txt'))&&JSON.parse(fs.readFileSync(path.join(dir,'ext_map.json'),'utf8'))[fakeId]===fakeName,{preserved:fs.existsSync(path.join(fakeDir,'keep.txt'))});}
  await send(privateWindow.ui,{cmd:'win_close'}).catch(()=>{});
  if(!baseline){
   for(let n=0;n<30&&fs.existsSync(privateWindow.profile);n++)await sleep(200);
   check('incognito_profile_cleanup',!fs.existsSync(privateWindow.profile),{removed:!fs.existsSync(privateWindow.profile)});
   await send(normal.ui,{cmd:'dev_tools'});let dev;
   for(let n=0;n<70&&!dev;n++){await sleep(150);const ts=await(await fetch(`http://127.0.0.1:${normal.port}/json/list`)).json();const target=ts.find(t=>t.url.includes('/devtools/inspector.html'));if(target)dev=await connect(target);}
   if(!dev)throw Error('Docked DevTools missing');connections.push(dev);await sleep(800);
   await send(dev,{cmd:'set_startup',mode:'restore'});await sleep(250);
   const settings=JSON.parse(fs.readFileSync(path.join(dir,'settings.json'),'utf8'));check('devtools_cannot_call_privileged_settings',settings.startup==='new',{startup:settings.startup});
   const appearance=await evaluate(dev,"(()=>{let seen;const original=window.onState;window.onState=s=>{seen=s;original(s)};window.ipc.postMessage(JSON.stringify({cmd:'ready'}));return new Promise(r=>setTimeout(()=>r({observed:!!seen,leaked:!!seen&&(!!seen.music_cfg||!!seen.tabs),keys:Object.keys(seen||{})}),250))})()");
   check('devtools_receives_no_tokens_or_tab_list',appearance.observed&&!appearance.leaked,appearance);
   await send(normal.ui,{cmd:'dev_close'});await send(normal.ui,{cmd:'go',text:attack});await sleep(400);
   const targets=await(await fetch(`http://127.0.0.1:${normal.port}/json/list`)).json();const target=targets.find(t=>t.url===attack);
   if(!target)throw Error('Untrusted page fixture missing');const page=await connect(target);connections.push(page);
   const exposed=await evaluate(page,"typeof window.ipc!=='undefined'");
   await evaluate(page,"window.chrome.webview.postMessage(JSON.stringify({cmd:'set_startup',mode:'restore'}));window.ipc?.postMessage(JSON.stringify({cmd:'set_startup',mode:'restore'}))");await sleep(200);
   const untrustedSettings=JSON.parse(fs.readFileSync(path.join(dir,'settings.json'),'utf8'));
   check('ordinary_page_cannot_call_native_commands',untrustedSettings.startup==='new',{wryTransportPresent:exposed,startup:untrustedSettings.startup});
  }
  await normal.ui.call('Page.navigate',{url:attack});await sleep(400);
  const href=await evaluate(normal.ui,'location.href');let settings=JSON.parse(fs.readFileSync(path.join(dir,'settings.json'),'utf8'));
  check('foreign_ui_navigation_and_ipc',baseline?href===attack&&settings.startup==='restore':href!==attack&&settings.startup==='new',{href,startup:settings.startup});
  await send(normal.ui,{cmd:'win_close'}).catch(()=>{});
 }finally{
  for(const c of connections)c.close();await sleep(1500);for(const p of children)if(p.exitCode===null)p.kill();server.close();
  fs.writeFileSync(path.join(dir,'security-results.json'),JSON.stringify({mode,results},null,2));
 }
 console.log(JSON.stringify({mode,passed:results.length,results},null,2));
})().catch(e=>{console.error(e);process.exitCode=1});
