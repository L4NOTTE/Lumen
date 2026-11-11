const http=require('node:http'),fs=require('node:fs');
const server=http.createServer((req,res)=>{
 if(req.url==='/'){res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>Download test</title><a href="/file">Download</a>');return;}
 const slow=req.url.startsWith('/slow'),fail=req.url.startsWith('/fail');
 const total=slow?8*1024*1024:1024*1024,chunk=Buffer.alloc(32768,65);
 res.writeHead(200,{'Content-Type':'application/octet-stream','Content-Disposition':`attachment; filename="${slow?'slow.bin':fail?'failed.bin':'sample.txt'}"`,'Content-Length':total});
 let sent=0;const timer=setInterval(()=>{if(fail&&sent>=524288){clearInterval(timer);res.destroy();return}res.write(chunk);sent+=chunk.length;if(sent>=total){clearInterval(timer);res.end()}},slow?80:40);
 res.on('close',()=>clearInterval(timer));
});
server.listen(0,'127.0.0.1',()=>fs.writeFileSync(process.argv[2],String(server.address().port)));
