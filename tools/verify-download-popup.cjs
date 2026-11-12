const fs=require('fs'),path=require('path'),http=require('http'),{spawn}=require('child_process');
const sleep=n=>new Promise(r=>setTimeout(r,n));
async function connect(t){const ws=new WebSocket(t.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});let id=0;const pending=new Map();ws.onmessage=e=>{const m=JSON.parse(e.data),p=pending.get(m.id);if(p){clearTimeout(p.timer);pending.delete(m.id);m.error?p.j(Error(JSON.stringify(m.error))):p.r(m.result)}};return {close:()=>ws.close(),call:(method,params={})=>new Promise((r,j)=>{const n=++id,timer=setTimeout(()=>{pending.delete(n);j(Error(method+' timeout'))},10000);pending.set(n,{r,j,timer});ws.send(JSON.stringify({id:n,method,params}))})};}
(async()=>{
const [exe,dir]=process.argv.slice(2);fs.mkdirSync(dir,{recursive:true});if(fs.existsSync(path.join(dir,'settings.json')))throw Error('Use fresh profile');fs.writeFileSync(path.join(dir,'settings.json'),'{"startup":"new","adblock":false}');
const payload=Buffer.alloc(262144,65);const server=http.createServer((q,r)=>{
if(q.url==='/file'){r.writeHead(200,{'Content-Type':'application/octet-stream','Content-Disposition':'attachment; filename="fixture.bin"','Content-Length':payload.length});let at=0;const timer=setInterval(()=>{r.write(payload.subarray(at,at+16384));at+=16384;if(at>=payload.length){clearInterval(timer);r.end()}},60);r.on('close',()=>clearInterval(timer));return;}
if(q.url==='/redirect'){r.writeHead(302,{Location:'/file'});r.end();return;}
r.setHeader('Content-Type','text/html');r.end('<title>Source</title><a id="download" target="_blank" href="/file">File</a><a id="redirect" target="_blank" href="/redirect">Redirect</a><a id="normal" target="_blank" href="/normal">Normal</a><a id="same" href="/file">Same</a>');});await new Promise(r=>server.listen(0,'127.0.0.1',r));const base='http://127.0.0.1:'+server.address().port;
const app=spawn(exe,['--secondary','--',base+'/source'],{windowsHide:true,env:{...process.env,LUMEN_DATA_DIR:dir,LUMEN_DOWNLOAD_DIR:path.join(dir,'files'),LUMEN_TEST_HIDDEN:'1',LUMEN_ENABLE_CDP:'1'}});let ui,page,port;const clients=[],results=[];
const evalJs=async(c,expression)=>(await c.call('Runtime.evaluate',{expression,userGesture:true,returnByValue:true})).result?.value;
const check=(name,ok)=>{results.push({name,passed:!!ok});if(!ok)throw Error(name)};
async function state(){return evalJs(ui,'({count:state.tabs.length,active:state.active})')}
try{
for(let n=0;n<120&&!ui;n++){await sleep(100);try{port=Number(fs.readFileSync(path.join(dir,'profile/lumen-debug-port'),'utf8'));for(const t of await(await fetch('http://127.0.0.1:'+port+'/json/list')).json()){if(t.type!=='page')continue;const c=await connect(t);if(await evalJs(c,"typeof state!=='undefined'&&!!document.getElementById('downloadsbtn')")){ui=c;clients.push(c)}else if(t.url===base+'/source'){page=c;clients.push(c)}else c.close();}}catch{}}
if(!page){const ts=await(await fetch('http://127.0.0.1:'+port+'/json/list')).json();page=await connect(ts.find(t=>t.url===base+'/source'));clients.push(page)}
const first=await state();
await evalJs(ui,"(()=>{const p=document.getElementById('customaccent');p.value='#237abc';p.dispatchEvent(new Event('change'));})()");
await sleep(300);
check('custom_accent_applies',await evalJs(ui,"state.set.accent==='#237abc'&&getComputedStyle(document.documentElement).getPropertyValue('--acc-l').trim()==='#237abc'"));
await evalJs(ui,"(()=>{const canvas=document.createElement('canvas');const data=atob(canvas.toDataURL().split(',')[1]);const dt=new DataTransfer();dt.items.add(new File([Uint8Array.from(data,c=>c.charCodeAt(0))],'test.png',{type:'image/png'}));const input=document.getElementById('file');input.files=dt.files;input.dispatchEvent(new Event('change'));})()");
for(let i=0;i<30&&!await evalJs(ui,'state.set.has_bg');i++)await sleep(100);
check('file_reader_background_works_under_csp',await evalJs(ui,"state.set.has_bg&&state.set.bg_mode==='custom'"));

const targets=await(await fetch('http://127.0.0.1:'+port+'/json/list')).json();
page=await connect(targets.find(t=>t.url===base+'/source'));clients.push(page);
const origin=await evalJs(page,'performance.timeOrigin');
for(const [index,id] of ['download','redirect','same'].entries()){
await evalJs(page,'document.getElementById('+JSON.stringify(id)+').click()');
let ready=false;for(let n=0;n<120;n++){await sleep(100);const records=fs.existsSync(path.join(dir,'downloads.json'))?JSON.parse(fs.readFileSync(path.join(dir,'downloads.json'),'utf8')):[];if(records.length>=index+1&&records.slice(0,index+1).every(r=>r.status==='completed')){ready=true;break}}
check(id+'_completes',ready);const current=await state();check(id+'_keeps_source_tab',current.count===1&&current.active===first.active);check(id+'_does_not_reload_source',await evalJs(page,'performance.timeOrigin')===origin);
}
await evalJs(page,"document.getElementById('normal').click()");await sleep(1600);const normal=await state();check('ordinary_target_blank_still_opens_foreground',normal.count===2&&normal.active!==first.active);
for(const file of fs.readdirSync(path.join(dir,'files')))check('download_integrity_'+file,fs.readFileSync(path.join(dir,'files',file)).equals(payload));
await evalJs(ui,"window.ipc.postMessage(JSON.stringify({cmd:'win_close'}))").catch(()=>{});
}finally{for(const c of clients)c.close();await sleep(800);if(app.exitCode===null)app.kill();server.close();fs.writeFileSync(path.join(dir,'popup-results.json'),JSON.stringify(results,null,2));}
console.log(results);
})().catch(e=>{console.error(e);process.exitCode=1});
