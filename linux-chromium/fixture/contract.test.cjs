const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),crypto=require('node:crypto'),vm=require('node:vm');
const root=__dirname;
test('MV3 package has a consistent identity and local-only fixture access',()=>{
  const manifest=JSON.parse(fs.readFileSync(path.join(root,'manifest.json'),'utf8'));
  assert.equal(manifest.manifest_version,3);
  const key=Buffer.from(manifest.key,'base64');crypto.createPublicKey({key,format:'der',type:'spki'});
  const id=[...crypto.createHash('sha256').update(key).digest().subarray(0,16)].map(n=>String.fromCharCode(97+(n>>4),97+(n&15))).join('');
  assert.equal(id,fs.readFileSync(path.join(root,'id.txt'),'utf8').trim());
  assert.deepEqual(manifest.host_permissions,['http://127.0.0.1/*']);
  for(const file of [manifest.background.service_worker,manifest.action.default_popup,...manifest.content_scripts.flatMap(s=>s.js)])assert.ok(fs.existsSync(path.join(root,file)));
});
test('worker rejects unrelated origins and propagates API failure',async()=>{
  let listener;
  const chrome={runtime:{onMessage:{addListener:fn=>listener=fn}},storage:{local:{set:async()=>{throw Error('denied')}}}};
  vm.runInNewContext(fs.readFileSync(path.join(root,'worker.js'),'utf8'),{chrome});
  assert.equal(listener({probe:true},{tab:{url:'https://example.com/'}},()=>assert.fail('untrusted origin')),undefined);
  const reply=await new Promise(resolve=>assert.equal(listener({probe:true},{tab:{url:'http://127.0.0.1:1234/'}},resolve),true));
  assert.match(reply.error,/denied/);
});
