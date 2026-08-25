/* Dragon Agent site engine: i18n (en/fa + rtl), hero terminal, copy, progress */
(function(){
"use strict";

let lang="en";
const STR={en:{},fa:{}};

const faDig=s=>String(s).replace(/[0-9](?![0-9]*\.?[0-9]*[a-z])/g,d=>"۰۱۲۳۴۵۶۷۸۹"[d]);

function applyLang(l){
  lang=l;
  document.documentElement.lang=l;
  document.documentElement.dir=(l==="fa")?"rtl":"ltr";
  const dict=STR[l];
  document.querySelectorAll("[data-i]").forEach(el=>{
    const v=dict[el.dataset.i];
    if(v!=null)el.innerHTML=v;
  });
  if(dict.title)document.title=dict.title;
  const b=document.querySelector(".lang-btn");
  if(b){b.textContent=l==="fa"?"EN":"فا";b.setAttribute("aria-label",
    l==="fa"?"switch to English":"تغییر به فارسی");}
  try{localStorage.setItem("dg-lang",l)}catch(e){}
  if(window.DG_ONLANG)window.DG_ONLANG(l);
}

/* boot language before first paint of translated nodes */
window.DG_LANG=function(strings){
  Object.assign(STR.en,strings.en||{});
  Object.assign(STR.fa,strings.fa||{});
  let saved=null;try{saved=localStorage.getItem("dg-lang")}catch(e){}
  applyLang(saved==="fa"?"fa":"en");
};
window.DG_T=k=>(STR[lang][k]!=null?STR[lang][k]:STR.en[k])||"";
window.DG_FADIG=faDig;

/* language toggle */
document.addEventListener("click",e=>{
  const b=e.target.closest(".lang-btn");
  if(b)applyLang(lang==="fa"?"en":"fa");
});

/* copy buttons */
document.addEventListener("click",e=>{
  const b=e.target.closest(".copy");
  if(!b)return;
  const pre=b.closest("pre");
  const txt=(pre?pre.querySelector("code"):null)||pre;
  navigator.clipboard.writeText(txt.textContent.replace(/^copy\n?/,"")).then(()=>{
    b.textContent=DG_T("copied");
    setTimeout(()=>{b.textContent=DG_T("copy")},1200);
  });
});

/* reading progress (docs) */
const prog=document.getElementById("progress");
if(prog)addEventListener("scroll",()=>{
  const h=document.documentElement;
  prog.style.width=(h.scrollTop/(h.scrollHeight-h.clientHeight)*100)+"%";
},{passive:true});

/* the signature: hero terminal types a real session.
   takes a function so it re-reads the script when language flips */
const REDUCE=matchMedia("(prefers-reduced-motion: reduce)").matches;
window.DG_TERMINAL=function(getScript){
  const body=document.getElementById("term-body");
  if(!body)return;
  const caret=document.createElement("span");caret.className="caret";
  let script=getScript(),li=0,ci=0,timer=null;
  function renderAll(){
    body.innerHTML="";
    script.forEach(l=>{
      const d=document.createElement("div");d.className="tl "+l.c;d.textContent=l.s;
      body.appendChild(d);
    });
  }
  function tick(){
    if(li>=script.length){
      timer=setTimeout(()=>{body.innerHTML="";li=0;ci=0;script=getScript();timer=setTimeout(tick,600)},5200);
      return;
    }
    const line=script[li];
    let el=body.children[li];
    if(!el){el=document.createElement("div");el.className="tl "+line.c;body.appendChild(el)}
    el.textContent=line.s.slice(0,++ci);
    el.appendChild(caret);
    if(ci<line.s.length){
      timer=setTimeout(tick,line.c==="tl-in"?(14+Math.random()*40):(line.c==="tl-out"?8:4));
    }else{
      li++;ci=0;
      timer=setTimeout(tick,line.c==="tl-in"?350:(line.c==="tl-mem"?550:180));
    }
  }
  function restart(){
    clearTimeout(timer);body.innerHTML="";li=0;ci=0;script=getScript();
    if(REDUCE)renderAll();else timer=setTimeout(tick,300);
  }
  if(REDUCE){renderAll()}
  else{
    timer=setTimeout(tick,900);
    const pageOnlang=window.DG_ONLANG;
    window.DG_ONLANG=l=>{restart();if(pageOnlang)pageOnlang(l)};
  }
};
})();
