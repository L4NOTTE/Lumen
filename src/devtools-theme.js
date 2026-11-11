// Runs in the local WebView2 frontend, including its frame; never on web pages.
(() => {
  if (location.hostname !== '127.0.0.1' || !location.pathname.startsWith('/devtools/')) return;
  let dark = matchMedia('(prefers-color-scheme:dark)').matches, accent = '#8b8dff';
  const roots = new WeakSet();
  // Small original stroke icons; native buttons, labels and actions remain intact.
  const drawings = {
    code:'<path d="m8 7-5 5 5 5m8-10 5 5-5 5m-2-14-4 18"/>',
    console:'<rect x="3" y="4" width="18" height="16" rx="3"/><path d="m7 9 3 3-3 3m6 0h4"/>',
    files:'<path d="M8 3h8l4 4v12a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm7 0v5h5M3 7v11a4 4 0 0 0 4 4"/>',
    network:'<path d="M4 7h16M4 17h16m-3-13 3 3-3 3M7 14l-3 3 3 3"/><circle cx="9" cy="7" r="1"/><circle cx="15" cy="17" r="1"/>',
    pulse:'<path d="M2 12h5l3-8 4 16 3-8h5"/>',
    chip:'<rect x="6" y="6" width="12" height="12" rx="3"/><path d="M9 2v4m6-4v4M9 18v4m6-4v4M2 9h4m-4 6h4m12-6h4m-4 6h4"/><rect x="10" y="10" width="4" height="4" rx="1"/>',
    database:'<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 4 16 4 16 0V5M4 12c0 4 16 4 16 0"/>',
    shield:'<path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6l8-3Z"/><path d="m8 12 3 3 5-6"/>',
    spark:'<path d="m12 2 2.7 7.3L22 12l-7.3 2.7L12 22l-2.7-7.3L2 12l7.3-2.7L12 2Z"/>',
    inspect:'<path d="M9 3H4v5m11-5h5v5M4 15v5h5m6-10 6 5-4 1-2 4-4-12 4 2Z"/>',
    devices:'<rect x="2" y="4" width="13" height="12" rx="2"/><path d="M5 20h7m-3-4v4"/><rect x="16" y="8" width="6" height="13" rx="2"/>',
    sliders:'<path d="M3 6h5m6 0h7M3 12h11m6 0h1M3 18h2m6 0h10"/><circle cx="11" cy="6" r="3"/><circle cx="17" cy="12" r="3"/><circle cx="8" cy="18" r="3"/>',
    search:'<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 5 5"/>',
    close:'<path d="m6 6 12 12M6 18 18 6"/>',
    plus:'<path d="M12 4v16M4 12h16"/>',
    refresh:'<path d="M20 10a8 8 0 1 0-1 8M20 3v7h-7"/>',
    external:'<path d="M14 3h7v7m0-7L11 13M9 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4"/>',
    dots:'<circle cx="12" cy="5" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="12" cy="19" r="1"/>',
    back:'<path d="m14 5-7 7 7 7"/>', forward:'<path d="m10 5 7 7-7 7"/>'
  };
  const svg = key => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${drawings[key]}</svg>`;
  const aliases={code:['edge-elements','code','elements'],console:['edge-console','terminal','console'],files:['edge-sources','sources','files'],network:['edge-network','network'],pulse:['edge-performance','performance'],chip:['edge-memory','memory'],database:['edge-application','database','application'],shield:['edge-security','security'],spark:['edge-welcome'],inspect:['select-element','mouse'],devices:['devices','touch-app'],sliders:['gear'],search:['search'],close:['cross'],plus:['plus'],refresh:['refresh'],dots:['dots-vertical'],back:['arrow-back'],forward:['arrow-forward']};
  const iconCss=Object.entries(aliases).map(([key,names])=>`${names.map(name=>`:host([name="${name}"]) span`).join(',')}{mask-image:url("data:image/svg+xml,${encodeURIComponent(svg(key).replace('currentColor','black'))}")!important}`).join('');
  let themeSheet, themeKey='';
  function css() {
    const bg=dark?'#191b22':'#f7f8fc',surface=dark?'#222530':'#ffffff',text=dark?'#dce3f0':'#26324a',line=dark?'#343b4c':'#dde3ee';
    const tag=dark?'#9dbbff':'#345cc8',attr=dark?'#c8a9f4':'#8051ac',value=dark?'#a6d9bd':'#276a4d',comment=dark?'#8792a8':'#68768e',number=dark?'#edbe91':'#9a5623';
    return `:root,:host{--sys-color-base-container:${bg}!important;--sys-color-base:${bg}!important;--sys-color-cdt-base:${bg}!important;--sys-color-cdt-base-container:${bg}!important;--sys-color-neutral-container:${surface}!important;--sys-color-on-base:${text}!important;--sys-color-on-surface-subtle:${comment}!important;--text-link:${accent}!important;--sys-color-surface:${surface}!important;--sys-color-surface1:${surface}!important;--sys-color-surface2:${bg}!important;--sys-color-on-surface:${text}!important;--sys-color-primary:${accent}!important;--sys-color-divider:${line}!important;--sys-color-neutral-outline:${line}!important;--sys-color-token-tag:${tag}!important;--sys-color-token-attribute:${attr}!important;--sys-color-token-attribute-value:${value}!important;--sys-color-token-string:${value}!important;--sys-color-token-comment:${comment}!important;--sys-color-token-number:${number}!important;--sys-color-token-keyword:${attr}!important;--sys-color-token-property:${tag}!important;--text-primary:${text}!important;--text-secondary:${comment}!important;--monospace-font-family:'Cascadia Code','Cascadia Mono',Consolas,monospace!important;--source-code-font-family:'Cascadia Code','Cascadia Mono',Consolas,monospace!important;--monospace-font-size:13px!important;--source-code-font-size:13px!important;--override-font-family:'Segoe UI',system-ui,sans-serif!important;color-scheme:${dark?'dark':'light'}!important}
body{background:${bg}!important;color:${text}!important}.root-view{top:44px!important;height:calc(100% - 44px)!important}
.tabbed-pane-header,.toolbar,.sidebar-pane-title,.data-grid th,.panel-toolbar{background:${surface}!important;color:${text}!important}.tabbed-pane-header{border-bottom:1px solid ${line}!important}.tabbed-pane-header-tab{border-radius:7px 7px 0 0}.tabbed-pane-header-tab.selected{box-shadow:inset 0 -2px ${accent}!important}
button{border-radius:6px!important}input,textarea,select{font-family:Segoe UI,system-ui,sans-serif!important;border-radius:6px!important}
.source-code,.monospace,.cm-editor,.cm-content,.cm-gutters{font-family:'Cascadia Code','Cascadia Mono',Consolas,monospace!important;font-size:13px!important;font-variant-ligatures:none;tab-size:2}.cm-content,.cm-line{line-height:1.65!important}.cm-content{padding-block:8px!important}.cm-activeLine{background:color-mix(in srgb,${accent} 9%,transparent)!important}.cm-gutters{background:${bg}!important;color:${comment}!important;border-right:1px solid ${line}!important}
.source-code{background-color:transparent!important}.elements-tree-outline,.styles-section{color:${text}!important;background-color:${bg}!important}.styles-section .header{background-color:${surface}!important}
.elements-disclosure li,.elements-tree-outline li{line-height:1.55!important;padding-top:2px!important;padding-bottom:2px!important}.elements-disclosure li.selected .selection,.elements-tree-outline li.selected>.selection{background:color-mix(in srgb,${accent} 17%,${bg})!important;border-block:1px solid color-mix(in srgb,${accent} 38%,transparent)!important}.elements-disclosure .webkit-html-attribute-value{color:${value}!important}.webkit-html-tag-name,.webkit-html-close-tag-name,.token-tag{color:${tag}!important}.webkit-html-attribute-name,.token-attribute,.token-keyword{color:${attr}!important}.webkit-html-comment,.token-comment{color:${comment}!important}.token-number{color:${number}!important}.token-string,.token-attribute-value{color:${value}!important}
.split-widget-sidebar,.widget.vbox,.data-grid{--sys-color-cdt-base-container:${bg};--sys-color-cdt-base:${bg};--sys-color-surface3:${surface};--sys-color-state-hover-on-subtle:color-mix(in srgb,${accent} 12%,transparent);--sys-color-state-focus-highlight:color-mix(in srgb,${accent} 22%,transparent)}
::-webkit-scrollbar{width:8px;height:8px}::-webkit-scrollbar-thumb{background:${line};border-radius:8px}::-webkit-scrollbar-track{background:${bg}}
${iconCss}`;
  }
  function style(root) {
    if(roots.has(root))return;
    if(!themeSheet){themeSheet=new CSSStyleSheet();themeSheet.replaceSync(css());}
    root.adoptedStyleSheets=[...root.adoptedStyleSheets,themeSheet];roots.add(root);
    root.querySelectorAll('*').forEach(el=>{if(el.shadowRoot)style(el.shadowRoot);});
  }
  const attach=Element.prototype.attachShadow;
  Element.prototype.attachShadow=function(options){const root=attach.call(this,options);style(root);return root;};
  let nativeThemeApplied, nativeThemePending=false, nativeThemeAttempts=0;
  async function syncNativeTheme() {
    if(nativeThemeApplied===dark || nativeThemePending || nativeThemeAttempts>=20)return;
    nativeThemePending=true;
    try {
      const Common=await import('./core/common/common.js');
      Common.Settings.Settings.instance().moduleSetting('ui-theme').set(dark?'dark':'default');
      nativeThemeApplied=dark;nativeThemeAttempts=0;
    } catch(_) {nativeThemeAttempts++;setTimeout(syncNativeTheme,250);}
    finally {nativeThemePending=false;}
  }
  window.onState=s=>{
    dark=s.incognito||s.set.theme==='dark'||(s.set.theme==='system'&&matchMedia('(prefers-color-scheme:dark)').matches);
    accent=dark?s.look.ad:s.look.al;
    const key=dark+':'+accent;
    if(themeSheet && key!==themeKey){themeKey=key;themeSheet.replaceSync(css());}
    document.documentElement.style.colorScheme=dark?'dark':'light';
    syncNativeTheme();
  };
  const send=cmd=>window.ipc.postMessage(JSON.stringify({cmd}));
  addEventListener('keydown',e=>{if(e.key==='F12'||(e.ctrlKey&&e.shiftKey&&e.key.toLowerCase()==='i')){e.preventDefault();e.stopImmediatePropagation();send('dev_close');}},true);
  addEventListener('DOMContentLoaded',()=>{
    style(document);document.title='Lumen DevTools 0.3';
    const titleNode=document.querySelector('title');
    if(titleNode)new MutationObserver(()=>{if(document.title!=='Lumen DevTools 0.3')document.title='Lumen DevTools 0.3';}).observe(titleNode,{childList:true,subtree:true,characterData:true});
    const host=document.createElement('div');host.id='lumen-devtools-host';host.dataset.design='2';host.style.cssText='position:fixed;left:0;right:0;top:0;height:44px;z-index:2147483647';
    const shadow=host.attachShadow({mode:'open'});
    const sheet=document.createElement('style');sheet.textContent='header{height:44px;box-sizing:border-box;display:flex;align-items:center;gap:6px;padding:0 9px;background:var(--sys-color-base-container);color:var(--sys-color-on-surface);border-bottom:1px solid var(--sys-color-divider);font:13px Segoe UI,sans-serif}b{flex:1;white-space:nowrap}small{opacity:.6}button{border:0;background:transparent;color:inherit;cursor:pointer;padding:5px 8px;border-radius:6px}button:hover{background:color-mix(in srgb,var(--sys-color-primary) 18%,transparent)}.grip{position:fixed;left:0;top:0;bottom:0;width:5px;cursor:col-resize;touch-action:none}.grip:hover{background:var(--sys-color-primary)}';sheet.textContent+='svg{width:18px;height:18px;display:block;flex:none}b>svg{color:var(--sys-color-primary)}button{width:28px;height:28px;padding:5px;display:grid;place-items:center}small{font-weight:400;font-size:12px;margin-left:4px}';shadow.append(sheet);
    const header=document.createElement('header'),title=document.createElement('b');title.innerHTML=svg('spark')+'<span>Lumen <small>DevTools</small></span>';title.style.cssText='display:flex;align-items:center;gap:9px';header.append(title);
    for(const [label,tip,cmd] of [['↻','Переподключить','dev_connect'],['↗','Отдельный инспектор','native_dev_tools'],['×','Закрыть (F12)','dev_close']]){const b=document.createElement('button');b.innerHTML=svg(cmd==='dev_connect'?'refresh':cmd==='native_dev_tools'?'external':'close');b.title=tip;b.setAttribute('aria-label',tip);b.onclick=()=>send(cmd);header.append(b);}
    shadow.append(header);const grip=document.createElement('div');grip.className='grip';grip.title='Изменить ширину';
    grip.onpointerdown=e=>{const start=e.screenX,width=innerWidth;grip.setPointerCapture(e.pointerId);grip.onpointermove=ev=>window.ipc.postMessage(JSON.stringify({cmd:'dev_width',width:width+start-ev.screenX}));const end=()=>{grip.onpointermove=null;grip.onpointerup=null;grip.onpointercancel=null;};grip.onpointerup=end;grip.onpointercancel=end;};shadow.append(grip);document.body.append(host);
    send('ready');
    const params = new URLSearchParams(location.search);
    if(params.has('lumenNode')) {
      (async()=>{
        try {
          const SDK=await import('./core/sdk/sdk.js'), Common=await import('./core/common/common.js');
          for(let attempt=0;attempt<40;attempt++) {
            const target=SDK.TargetManager.TargetManager.instance().primaryPageTarget();
            if(target?.model(SDK.DOMModel.DOMModel)){
              try {
                await target.model(SDK.DOMModel.DOMModel).requestDocument();
                const node=await new SDK.DOMModel.DeferredDOMNode(target,+params.get('lumenNode')).resolvePromise();
                if(node){await Common.Revealer.reveal(node);window.__lumenInspectStatus='selected';break;}
              } catch(error) { window.__lumenInspectStatus=String(error); }
            }
            await new Promise(resolve=>setTimeout(resolve,250));
          }
        } catch(error) { window.__lumenInspectStatus=String(error);console.error('Lumen: cannot reveal inspected element',error); }
      })();
    }
  });
})();
