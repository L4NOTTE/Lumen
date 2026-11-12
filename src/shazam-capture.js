// Official Shazam only: explicit recognition click starts native WASAPI loopback.
(()=>{
 if(location.protocol!=='chrome-extension:'||location.hostname!=='mmioliijnhnoblpgimnlajmefafdfilb'||!window.ipc)return;
 const send=cmd=>window.ipc.postMessage(JSON.stringify({cmd}));
 let context=null,destination=null,stream=null,next=0,active=false;
 const captured=new WeakSet(),create=AudioContext.prototype.createMediaStreamSource;
 AudioContext.prototype.createMediaStreamSource=function(s){const node=create.call(this,s);if(captured.has(s)){const connect=node.connect;node.connect=function(target,...args){if(target instanceof AudioDestinationNode)return target;return connect.call(this,target,...args);};}return node;};
 function stop(){if(!active)return;active=false;send('capture_stop');if(context)context.close().catch(()=>{});context=null;destination=null;stream=null;}
 window.__lumenAudio=(rate,b64)=>{
   if(!active||!context)return;
   const bytes=Uint8Array.from(atob(b64),c=>c.charCodeAt(0)),data=new Float32Array(bytes.buffer);
   const buffer=context.createBuffer(1,data.length,rate);buffer.copyToChannel(data,0);
   const source=context.createBufferSource();source.buffer=buffer;source.connect(destination);
   next=Math.max(next,context.currentTime+.06);if(next-context.currentTime>1)next=context.currentTime+.06;
   source.start(next);next+=buffer.duration;source.onended=()=>source.disconnect();
 };
 window.__lumenAudioEnd=stop;
 window.__lumenAudioError=error=>{stop();const banner=document.createElement('div');banner.textContent='Lumen: не удалось получить системный звук: '+error;banner.style.cssText='padding:12px;background:#35212a;color:white;font:13px Segoe UI';document.body.prepend(banner);};
 const capture=(_options,callback)=>{
   stop();active=true;context=new AudioContext();context.resume().catch(()=>{});destination=context.createMediaStreamDestination();stream=destination.stream;captured.add(stream);next=0;
   for(const track of stream.getAudioTracks()){const end=track.stop.bind(track);track.stop=()=>{end();stop();};}
   send('capture_start');callback(stream);
 };
 if(!chrome.tabCapture)chrome.tabCapture={};chrome.tabCapture.capture=capture;
 addEventListener('pagehide',stop);addEventListener('beforeunload',stop);
})();
