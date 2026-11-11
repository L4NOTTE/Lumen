const fs=require('fs'),path=require('path'),http=require('http'),{spawn,execFileSync}=require('child_process');
const sleep=n=>new Promise(r=>setTimeout(r,n));
async function connect(t){const ws=new WebSocket(t.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});let id=0;const pending=new Map();ws.onmessage=e=>{const m=JSON.parse(e.data),p=pending.get(m.id);if(p){clearTimeout(p.timer);pending.delete(m.id);m.error?p.j(Error(JSON.stringify(m.error))):p.r(m.result)}};return {close:()=>ws.close(),call:(method,params={})=>new Promise((r,j)=>{const n=++id,timer=setTimeout(()=>{pending.delete(n);j(Error(method+' timeout'))},10000);pending.set(n,{r,j,timer});ws.send(JSON.stringify({id:n,method,params}))})};}
(async()=>{
const [exe,dir]=process.argv.slice(2);fs.mkdirSync(dir,{recursive:true});if(fs.existsSync(path.join(dir,'settings.json')))throw Error('Use fresh profile');fs.writeFileSync(path.join(dir,'settings.json'),'{"startup":"new","adblock":false}');
const server=http.createServer((q,r)=>r.end('<title>Restart fixture</title>fixture'));await new Promise(r=>server.listen(0,'127.0.0.1',r));const url='http://127.0.0.1:'+server.address().port+'/restart';
const env={...process.env,LUMEN_DATA_DIR:dir,LUMEN_TEST_HIDDEN:'1'};delete env.LUMEN_ENABLE_CDP;
const app=spawn(exe,['--developer-mode','--url',url],{env,windowsHide:true});let ui,second,third;const checks=[];
const evaluate=async(c,expression)=>(await c.call('Runtime.evaluate',{expression,returnByValue:true})).result?.value;
async function findUI(){for(let i=0;i<120;i++){await sleep(100);try{const port=Number(fs.readFileSync(path.join(dir,'profile/lumen-debug-port'),'utf8'));for(const t of await(await fetch('http://127.0.0.1:'+port+'/json/list')).json()){if(t.type!=='page')continue;let c=await connect(t);if(await evaluate(c,"typeof state!=='undefined'&&!!document.getElementById('devmode')"))return c;c.close()}}catch{}}throw Error('No UI')}
function child(pid){const text=execFileSync('powershell.exe',['-NoProfile','-Command',"Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'lumen.exe' -and $_.CommandLine -like '*--restart-after "+pid+" *' } | Select-Object -ExpandProperty ProcessId"],{encoding:'utf8',windowsHide:true}).trim();return Number(text.split(/\s+/)[0])||null}
try{
ui=await findUI();await sleep(500);
for(const width of [320,800]){
await ui.call('Emulation.setDeviceMetricsOverride',{width,height:80,deviceScaleFactor:1,mobile:false});
await evaluate(ui,"window.onToast('DevTools выключен. Включите режим разработчика в настройках браузера.')");await sleep(250);
const rect=await evaluate(ui,"(()=>{const r=document.getElementById('toast').getBoundingClientRect();return {top:r.top,bottom:r.bottom,left:r.left,right:r.right}})()");
if(rect.top<0||rect.bottom>80||rect.left<0||rect.right>width)throw Error('Toast clipped: '+JSON.stringify(rect));
}
await ui.call('Emulation.clearDeviceMetricsOverride');checks.push('toast_fits_320_and_800px_toolbar');
if(!await evaluate(ui,'state.developer_mode'))throw Error('Developer status missing');
await evaluate(ui,"window.confirm=()=>false;document.getElementById('devmode').click()");await sleep(200);if(app.exitCode!==null)throw Error('Cancelled restart closed app');checks.push('cancel_keeps_window');
await evaluate(ui,"window.ipc.postMessage(JSON.stringify({cmd:'restart_developer',on:true}))");ui.close();ui=null;
for(let i=0;i<60&&!second;i++){await sleep(100);second=child(app.pid)}if(!second)throw Error('Restart child missing');
ui=await findUI();if(!await evaluate(ui,'state.developer_mode&&state.tabs.some(t=>t.url==='+JSON.stringify(url)+')'))throw Error('Session not restored in developer mode');checks.push('restart_restores_session_with_startup_new');
await evaluate(ui,"window.confirm=()=>true;document.getElementById('devmode').click()");ui.close();ui=null;
for(let i=0;i<60&&!third;i++){await sleep(100);third=child(second)}if(!third)throw Error('Secure restart child missing');
await sleep(3500);process.kill(third,0);const port=Number(fs.readFileSync(path.join(dir,'profile/lumen-debug-port'),'utf8'));let open=false;try{await fetch('http://127.0.0.1:'+port+'/json/list');open=true}catch{}if(open)throw Error('CDP still open after disabling');checks.push('settings_button_disables_cdp_after_restart');
fs.writeFileSync(path.join(dir,'restart-results.json'),JSON.stringify(checks,null,2));console.log(checks);
}finally{ui?.close();for(const pid of [third,second,app.pid])if(pid)try{process.kill(pid)}catch{}server.close()}
})().catch(e=>{console.error(e);process.exitCode=1});
