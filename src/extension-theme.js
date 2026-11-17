// Restrained shared controls: do not rewrite extension layouts or content.
(() => {
  if(location.protocol!=='chrome-extension:') return;
  const mount=()=>{
    if(document.getElementById('lumen-extension-controls'))return;
    const style=document.createElement('style');style.id='lumen-extension-controls';
    style.textContent=`
      :root{accent-color:#8274ec}
      body{font-family:'Segoe UI',system-ui,sans-serif}
      button,input,select,textarea{font-family:inherit}
      button,input,select,textarea{border-radius:7px}
      button:focus-visible,input:focus-visible,select:focus-visible,textarea:focus-visible{outline:2px solid #8274ec;outline-offset:2px}
      ::-webkit-scrollbar{width:8px;height:8px}
      ::-webkit-scrollbar-thumb{background:#8888;border-radius:8px}
      @media(prefers-reduced-motion:reduce){*{scroll-behavior:auto!important}}
    `;(document.head||document.documentElement).append(style);
  };
  if(document.documentElement)mount();else document.addEventListener('DOMContentLoaded',mount,{once:true});
})();
