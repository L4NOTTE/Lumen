const {spawn}=require('node:child_process');
const fs=require('node:fs'), path=require('node:path'), os=require('node:os'), http=require('node:http'), crypto=require('node:crypto');
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const [exe,root]=process.argv.slice(2);
const normalSecret='normal_'+crypto.randomBytes(12).toString('hex');
const oldSecret='legacy_'+crypto.randomBytes(12).toString('hex');
const privateSecret='private_'+crypto.randomBytes(12).toString('hex');
const dir=path.join(root,'profile-fixture');
fs.mkdirSync(dir,{recursive:true});
fs.writeFileSync(path.join(dir,'settings.json'),JSON.stringify({adblock:false,startup:'new'}));
const oldUrl='https://example.test/old?v=123&auth='+oldSecret;
fs.writeFileSync(path.join(dir,'history.jsonl'),JSON.stringify({u:oldUrl,t:'old history',ts:1})+'\n');
fs.writeFileSync(path.join(dir,'downloads.json'),JSON.stringify([{id:'old',name:'old.txt',path:path.join(root,'old.txt'),url:oldUrl,received:1,total:1,status:'completed',created:1,error:null}]));
const checks=[],received={};
const check=(name,passed,details={})=>checks.push({name,passed:!!passed,details});
const page=script=>'<!doctype html><title>Audit fixture</title><script>'+script+'</script>';
const scriptLiteral=v=>JSON.stringify(v);
let origin, otherOrigin;
const sink=http.createServer((req,res)=>{
  received.external={referer:req.headers.referer??null,cookie:req.headers.cookie??null};
  res.end('ok');
});
const server=http.createServer((req,res)=>{
  const u=new URL(req.url,'http://localhost');
  res.setHeader('Content-Type','text/html; charset=utf-8');
  if(u.pathname==='/seed'){
    res.setHeader('Set-Cookie','audit='+normalSecret+'; Path=/; SameSite=Lax');
    res.end(page('localStorage.audit='+scriptLiteral(normalSecret)+'; location.replace("/regular?auth="+encodeURIComponent(localStorage.audit));'));
  } else if(u.pathname==='/regular'){
    received.normalCookie=req.headers.cookie??'';
    res.end(page('fetch("/normal-ready?storage="+encodeURIComponent(localStorage.audit));'));
  } else if(u.pathname==='/normal-ready'){
    received.normalStorage=u.searchParams.get('storage');
    res.end('ok');
  } else if(u.pathname==='/private'){
    received.privateCookie=req.headers.cookie??'';
    res.end(page(`(async()=>{
      await fetch('/private-before?storage='+encodeURIComponent(localStorage.audit||''));
      localStorage.audit=${scriptLiteral(privateSecret)};
      document.cookie='audit='+localStorage.audit+'; Path=/; SameSite=Lax';
      for(const command of [{cmd:'set_startup',mode:'restore'},{cmd:'win_close'}]){
        try { window.ipc?.postMessage(JSON.stringify(command)); } catch {}
        try { window.chrome?.webview?.postMessage(JSON.stringify(command)); } catch {}
      }
      location.replace('/private-after?auth='+encodeURIComponent(localStorage.audit));
    })();`));
  } else if(u.pathname==='/private-before'){
    received.privateStorage=u.searchParams.get('storage');
    res.end('ok');
  } else if(u.pathname==='/private-after'){
    received.privateAfterCookie=req.headers.cookie??'';
    res.end(page(`(async()=>{
      const geo=await new Promise(resolve=>navigator.geolocation.getCurrentPosition(()=>resolve('allowed'),e=>resolve(e.code===1?'denied':'unavailable'),{timeout:3500}));
      await fetch('/private-ready?geo='+geo);
      await fetch(${scriptLiteral(otherOrigin+'/sink')},{mode:'no-cors'});
      location.href='https://lumen.invalid/s?q='+encodeURIComponent(location.origin+'/unexpected');
    })();`));
  } else if(u.pathname==='/private-ready'){
    received.geo=u.searchParams.get('geo');
    res.end('ok');
  } else if(u.pathname==='/unexpected'){
    received.forged=true;
    res.end('unexpected');
  } else res.end('ok');
});
const launch=pathName=>spawn(exe,[...(pathName==='/private'?['--incognito']:[]),'--url',origin+pathName],{
  windowsHide:true,env:{...process.env,LUMEN_DATA_DIR:dir,LUMEN_DOWNLOAD_DIR:path.join(root,'downloads'),LUMEN_TEST_HIDDEN:'1',LUMEN_ENABLE_CDP:'1',WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-port=39897'}
});
async function wait(predicate, label){
  for(let i=0;i<100;i++){if(predicate())return;await sleep(100);}
  throw Error('Missing fixture event: '+label);
}
function contains(file,token){try{return fs.readFileSync(file,'utf8').includes(token)}catch{return false}}
(async()=>{
 await new Promise(r=>server.listen(0,'127.0.0.1',r));
 await new Promise(r=>sink.listen(0,'127.0.0.1',r));
 origin='http://127.0.0.1:'+server.address().port;
 otherOrigin='http://127.0.0.1:'+sink.address().port;
 let normal,priv;
 try{
  normal=launch('/seed');
  await wait(()=>received.normalStorage!==undefined,'regular storage');
  check('fixture_regular_cookie_and_storage',received.normalCookie.includes(normalSecret)&&received.normalStorage===normalSecret);
  const historyFile=path.join(dir,'history.jsonl'), sessionFile=path.join(dir,'session.json'), downloadsFile=path.join(dir,'downloads.json');
  await wait(()=>contains(historyFile,'/regular')&&contains(sessionFile,'/regular'),'browser history and session flushed');
  check('regular_url_token_absent_in_history',!contains(historyFile,normalSecret));
  check('regular_url_token_absent_in_session',!contains(sessionFile,normalSecret));
  check('legacy_history_url_redacted',contains(historyFile,'/old?v=123')&&!contains(historyFile,oldSecret));
  check('legacy_download_url_redacted',contains(downloadsFile,'/old?v=123')&&!contains(downloadsFile,oldSecret));
  priv=launch('/private');
  await wait(()=>received.external&&received.geo,'private permissions and cross-origin request');
  await sleep(700);
  check('private_cannot_read_regular_cookie',!received.privateCookie.includes(normalSecret));
  check('private_cannot_read_regular_storage',received.privateStorage==='');
  check('fixture_private_cookie_written',received.privateAfterCookie.includes(privateSecret));
  check('private_url_not_in_regular_history',!contains(path.join(dir,'history.jsonl'),privateSecret));
  check('private_url_not_in_regular_session',!contains(path.join(dir,'session.json'),privateSecret));
  check('cross_origin_referrer_omits_secret',!received.external.referer?.includes(privateSecret),{referer:received.external.referer});
  check('geolocation_denied',received.geo==='denied');
  check('page_cannot_close_browser',priv.exitCode===null);
  check('page_cannot_change_startup',JSON.parse(fs.readFileSync(path.join(dir,'settings.json'))).startup==='new');
  check('forged_start_page_command_blocked',!received.forged);
  let listener=false;
  try{const r=await fetch('http://127.0.0.1:39897/json/list',{signal:AbortSignal.timeout(1200)});listener=r.ok;}catch{}
  check('requested_cdp_endpoint_inactive',!listener&&!fs.existsSync(path.join(dir,'profile/lumen-debug-port')));
  const privateProfile=path.join(os.tmpdir(),'Lumen-incognito-'+priv.pid);
  check('private_profile_exists_during_session',fs.existsSync(privateProfile));
  priv.kill();
  for(let attempt=0;attempt<120&&fs.existsSync(privateProfile);attempt++)await sleep(100);
  check('forced_exit_removes_private_profile',!fs.existsSync(privateProfile));
  const hits=[];
  let scanned=0;
  function scan(folder){
    if(scanned>64*1024*1024)return;
    let items;try{items=fs.readdirSync(folder,{withFileTypes:true});}catch{return;}
    for(const item of items){
      const p=path.join(folder,item.name);
      if(item.isDirectory()){scan(p);continue;}
      if(!item.isFile())continue;
      try{
        const size=fs.statSync(p).size;if(size>8*1024*1024||scanned+size>64*1024*1024)continue;
        const bytes=fs.readFileSync(p);scanned+=size;
        if(bytes.includes(Buffer.from(privateSecret))||bytes.includes(Buffer.from(privateSecret,'utf16le')))hits.push(path.relative(privateProfile,p));
      }catch{}
    }
  }
  scan(privateProfile);
  check('private_canary_absent_from_readable_residual_files',hits.length===0,{scannedBytes:scanned,matchingFiles:hits});

 }finally{
  for(const child of [priv,normal])if(child&&child.exitCode===null)child.kill();
  server.close();sink.close();
  fs.writeFileSync(path.join(root,'blackbox-results.json'),JSON.stringify(checks,null,2));
 }
 console.log(JSON.stringify(checks,null,2));
 if(checks.some(result=>!result.passed))process.exitCode=1;
})().catch(e=>{console.error(e);process.exitCode=1});
