// Presentation only: never starts capture or changes extension actions.
(() => {
  if(location.protocol!=='chrome-extension:'||location.hostname!=='mmioliijnhnoblpgimnlajmefafdfilb')return;
  const apply=()=>{
    const dark=window.__lumenPopupTheme==='dark'||(window.__lumenPopupTheme!=='light'&&matchMedia('(prefers-color-scheme:dark)').matches);
    document.documentElement.dataset.lumenTheme=dark?'dark':'light';
  };
  const css=`
    :root{--lum-bg:#f3f4f6;--lum-card:#fff;--lum-text:#1f2328;--lum-muted:#687080;--lum-line:#dfe3ea;--lum-accent:#6558d9;color-scheme:light}
    :root[data-lumen-theme=dark]{--lum-bg:#1b1c20;--lum-card:#25262b;--lum-text:#e6e7ea;--lum-muted:#a0a6b5;--lum-line:#373b45;--lum-accent:#9b91ff;color-scheme:dark}
    html,body{margin:0!important;width:100%!important;min-width:0!important;max-width:100%!important;background:var(--lum-bg)!important;color:var(--lum-text)!important;overflow-x:hidden!important;font-family:'Segoe UI Variable','Segoe UI',sans-serif!important}
    *{box-sizing:border-box}body{padding:0!important}#app-container,.App{width:100%!important;max-width:100%!important;min-width:0!important}
    p,span,h1,h2,h3,h4{font-family:'Segoe UI Variable','Segoe UI',sans-serif!important}h1,h2,h3,h4{color:var(--lum-text)!important}
    .WTxO9QoMUPzY85sxePla,.UHL5BVTJJbsfG2hGTVvv,.hqPiV58pa21f7uh828xl,.fkkc5GcLOKP8FbLtZ6gb,.vhnCJCu4noKd1uDG9Uow,.FUz9RNDzlasgMu9CuLiJ{width:100%!important;max-width:100%!important;min-width:0!important;margin-right:0!important;box-sizing:border-box!important}
    .UHL5BVTJJbsfG2hGTVvv,.hqPiV58pa21f7uh828xl{left:0!important;right:0!important;padding-left:16px!important;padding-right:16px!important}
    .FUz9RNDzlasgMu9CuLiJ{background:linear-gradient(135deg,#35345e,#243f58)!important;border-bottom:1px solid var(--lum-line)}
    .VRUhPzbkVeLTTUR5ydTO,.goUMNll6CThUwWWlCPqO{width:100%!important;max-width:100%!important;background:linear-gradient(135deg,#35345e,#243f58)!important;background-image:linear-gradient(135deg,#35345e,#243f58)!important;color:#f4f6ff!important}
    .PxU3ZWOF7Hsmx6yLeNR0{gap:16px!important;max-width:100%!important;min-width:0!important}
    .ud8U9hWDQ4geAHT0H9ui{right:14px!important;width:auto!important;min-width:100px!important;max-width:none!important;flex-shrink:0!important;white-space:nowrap!important;overflow-wrap:normal!important;word-break:normal!important;font-size:12px!important;padding:7px 12px!important;box-sizing:border-box!important}
    .S4Qz81BGxxpjuGEo02kM{border-color:var(--lum-line)!important;background:var(--lum-bg)!important;color:var(--lum-text)!important;padding:0 16px!important}
    .RLHtwz_RlwPoJIR5A33T{background:var(--lum-card)!important;color:var(--lum-text)!important;max-height:calc(100vh - 270px)!important;overflow-y:auto!important;overflow-x:hidden!important;scrollbar-gutter:stable}
    .RLHtwz_RlwPoJIR5A33T *{color:inherit!important;border-color:var(--lum-line)!important}
    .RLHtwz_RlwPoJIR5A33T img{border-radius:9px!important;object-fit:cover}
    .cFtk8eToFeeOJdJA14kL{font-size:18px!important;line-height:1.3!important;overflow-wrap:anywhere}
    .utfAy3uwYPlnpYZP7gfL{line-height:1.4!important;overflow-wrap:anywhere}
    button{font-family:inherit!important;border-radius:8px!important;max-width:100%!important}
    button,button:hover,button:active,button:focus{transform:none!important;box-shadow:none!important;outline:none!important;transition:none!important}
    img{max-width:100%}::-webkit-scrollbar{width:8px}::-webkit-scrollbar-track{background:var(--lum-bg)}::-webkit-scrollbar-thumb{background:#687080;border:2px solid var(--lum-bg);border-radius:6px}
    @media(max-width:390px){.cFtk8eToFeeOJdJA14kL{font-size:16px!important}}
  `;
  const install=()=>{apply();const sheet=new CSSStyleSheet();sheet.replaceSync(css);document.adoptedStyleSheets=[...document.adoptedStyleSheets,sheet];};
  if(document.readyState==='loading')addEventListener('DOMContentLoaded',install,{once:true});else install();
  matchMedia('(prefers-color-scheme:dark)').addEventListener('change',apply);
})();
