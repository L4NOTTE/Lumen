const http=require('node:http'),fs=require('node:fs');
const ports=[],servers=[];
for(let i=0;i<2;i++){
 const server=http.createServer((req,res)=>{res.setHeader('Content-Type','text/html; charset=utf-8');res.end('<!doctype html><title>Zoom fixture</title><p id="fixture">Lumen zoom persistence fixture</p>');});
 servers.push(server);server.listen(0,'127.0.0.1',()=>{ports[i]=server.address().port;if(ports.filter(Boolean).length===2)fs.writeFileSync(process.argv[2],JSON.stringify(ports));});
}
