// Runs in the local WebView2 frontend, including its frame; never on web pages.
(() => {
  if (location.hostname !== '127.0.0.1' || !location.pathname.startsWith('/devtools/')) return;
  let dark = matchMedia('(prefers-color-scheme:dark)').matches, accent = '#8b8dff';
  const roots = new Set();
  function css() {
    const bg=dark?'#1b1c20':'#f3f4f6',surface=dark?'#25262b':'#ffffff',text=dark?'#e6e7ea':'#1f2328',line=dark?'#34363e':'#dce0e6';
    return `:root,:host {--sys-color-base-container:${bg}!important;--sys-color-base:${bg}!important;--sys-color-surface:${surface}!important;--sys-color-surface1:${surface}!important;--sys-color-surface2:${bg}!important;--sys-color-on-surface:${text}!important;--sys-color-primary:${accent}!important;--sys-color-divider:${line}!important;--sys-color-neutral-outline:${line}!important;--override-font-family:'Segoe UI',system-ui,sans-serif!important;color-scheme:${dark?'dark':'light'}!important}body{background:${bg}!important;color:${text}!important}.root-view{top:40px!important;height:calc(100% - 40px)!important}.tabbed-pane-header{background:${surface}!important;border-bottom:1px solid ${line}!important}.tabbed-pane-header-tab.selected{box-shadow:inset 0 -2px ${accent}!important}button{border-radius:6px!important}`;
  }
  function style(root) {
    if(roots.has(root))return;const sheet=new CSSStyleSheet();sheet.replaceSync(css());root.adoptedStyleSheets=[...root.adoptedStyleSheets,sheet];roots.add(root);root.__lumenTheme=sheet;
    root.querySelectorAll('*').forEach(el=>{if(el.shadowRoot)style(el.shadowRoot);});
  }
  const attach=Element.prototype.attachShadow;
  Element.prototype.attachShadow=function(options){const root=attach.call(this,options);style(root);return root;};
  window.onState=s=>{
    dark=s.incognito||s.set.theme==='dark'||(s.set.theme==='system'&&matchMedia('(prefers-color-scheme:dark)').matches);
    accent=dark?s.look.ad:s.look.al;
    for(const root of roots)root.__lumenTheme.replaceSync(css());
    document.documentElement.style.colorScheme=dark?'dark':'light';
  };
  const send=cmd=>window.ipc.postMessage(JSON.stringify({cmd}));
  addEventListener('keydown',e=>{if(e.key==='F12'||(e.ctrlKey&&e.shiftKey&&e.key.toLowerCase()==='i')){e.preventDefault();e.stopImmediatePropagation();send('dev_close');}},true);
  addEventListener('DOMContentLoaded',()=>{
    style(document);document.title='Lumen DevTools 0.3';
    const titleNode=document.querySelector('title');
    if(titleNode)new MutationObserver(()=>{if(document.title!=='Lumen DevTools 0.3')document.title='Lumen DevTools 0.3';}).observe(titleNode,{childList:true,subtree:true,characterData:true});
    const host=document.createElement('div');host.id='lumen-devtools-host';host.style.cssText='position:fixed;left:0;right:0;top:0;height:40px;z-index:2147483647';
    const shadow=host.attachShadow({mode:'open'});
    const sheet=document.createElement('style');sheet.textContent='header{height:40px;box-sizing:border-box;display:flex;align-items:center;gap:6px;padding:0 9px;background:var(--sys-color-base-container);color:var(--sys-color-on-surface);border-bottom:1px solid var(--sys-color-divider);font:13px Segoe UI,sans-serif}b{flex:1;white-space:nowrap}small{opacity:.6}button{border:0;background:transparent;color:inherit;cursor:pointer;padding:5px 8px;border-radius:6px}button:hover{background:color-mix(in srgb,var(--sys-color-primary) 18%,transparent)}.grip{position:fixed;left:0;top:0;bottom:0;width:5px;cursor:col-resize;touch-action:none}.grip:hover{background:var(--sys-color-primary)}';shadow.append(sheet);
    const header=document.createElement('header'),title=document.createElement('b');title.textContent='</> Lumen DevTools';header.append(title);
    for(const [label,tip,cmd] of [['↻','Переподключить','dev_connect'],['↗','Отдельный инспектор','native_dev_tools'],['×','Закрыть (F12)','dev_close']]){const b=document.createElement('button');b.textContent=label;b.title=tip;b.onclick=()=>send(cmd);header.append(b);}
    shadow.append(header);const grip=document.createElement('div');grip.className='grip';grip.title='Изменить ширину';
    grip.onpointerdown=e=>{const start=e.screenX,width=innerWidth;grip.setPointerCapture(e.pointerId);grip.onpointermove=ev=>window.ipc.postMessage(JSON.stringify({cmd:'dev_width',width:width+start-ev.screenX}));const end=()=>{grip.onpointermove=null;grip.onpointerup=null;grip.onpointercancel=null;};grip.onpointerup=end;grip.onpointercancel=end;};shadow.append(grip);document.body.append(host);
    send('ready');
  });
})();
