// WebView2 does not provide Chrome's tabCapture/action invocation context.
// Only the official Shazam popup gets this explicit-consent display-audio adapter.
(() => {
  if (location.protocol !== 'chrome-extension:' || location.hostname !== 'mmioliijnhnoblpgimnlajmefafdfilb') return;
  const captured = new WeakSet();
  const create = AudioContext.prototype.createMediaStreamSource;
  AudioContext.prototype.createMediaStreamSource = function(stream) {
    const node = create.call(this,stream);
    if(captured.has(stream)) {
      const connect=node.connect;
      // tabCapture normally mutes the original tab; display audio does not.
      // Do not route captured system audio back to the speakers (feedback).
      node.connect=function(destination,...args){if(destination instanceof AudioDestinationNode)return destination;return connect.call(this,destination,...args);};
    }
    return node;
  };
  let pending=null,banner=null;
  function message(text) {
    if(!document.body){addEventListener('DOMContentLoaded',()=>message(text),{once:true});return;}
    if(!banner){banner=document.createElement('div');banner.style.cssText='position:fixed;z-index:2147483647;top:0;left:0;right:0;padding:12px;background:#202337;color:#f2f4ff;font:12px/1.5 Segoe UI,sans-serif;border-bottom:1px solid #687dff';document.body.prepend(banner);document.body.style.paddingTop='104px';}
    banner.replaceChildren();const label=document.createElement('div');label.textContent=text;banner.append(label);
    const button=document.createElement('button');button.textContent='Выбрать источник звука';button.style.cssText='margin-top:6px;padding:6px 12px;border:0;border-radius:7px;background:#8b8dff;color:#141628;cursor:pointer';button.onclick=choose;banner.append(button);
  }
  async function choose(){
    if(!pending){message('Сначала нажмите кнопку распознавания Shazam, затем выберите источник звука.');return;}
    try {
      if(!navigator.mediaDevices?.getDisplayMedia)throw new Error('Захват экрана недоступен в этой версии WebView2.');
      const display=await navigator.mediaDevices.getDisplayMedia({video:true,audio:true,systemAudio:'include'});
      const audio=display.getAudioTracks();
      if(!audio.length){display.getTracks().forEach(t=>t.stop());throw new Error('Источник не передал звук. Выберите экран и включите передачу системного звука в диалоге.');}
      const stream=new MediaStream(audio);captured.add(stream);
      let stopped=false;const stop=()=>{if(stopped)return;stopped=true;display.getTracks().forEach(t=>t.stop());};
      for(const track of audio){const original=track.stop.bind(track);track.stop=()=>{original();stop();};track.addEventListener('ended',stop,{once:true});}
      for(const track of display.getVideoTracks())track.addEventListener('ended',stop,{once:true});
      addEventListener('pagehide',stop,{once:true});
      if(banner)banner.remove();banner=null;document.body.style.paddingTop='';
      const callback=pending;pending=null;callback(stream);
    } catch(error) { message(error.name==='NotAllowedError'?'Выбор отменён или доступ не разрешён. Можно повторить выбор источника.':String(error.message||error)); }
  }
  const capture=(_options,callback)=>{pending=callback;message('Lumen: выберите источник и разрешите передачу звука в системном диалоге. Захват начинается только после вашего выбора.');};
  try { if(!chrome.tabCapture)chrome.tabCapture={};chrome.tabCapture.capture=capture;if(chrome.tabCapture.capture!==capture)Object.defineProperty(chrome.tabCapture,'capture',{value:capture}); }
  catch(error){message('Не удалось подключить совместимый захват: '+error.message);}
})();
