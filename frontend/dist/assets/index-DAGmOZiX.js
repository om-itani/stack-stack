(function(){let e=document.createElement(`link`).relList;if(e&&e.supports&&e.supports(`modulepreload`))return;for(let e of document.querySelectorAll(`link[rel="modulepreload"]`))n(e);new MutationObserver(e=>{for(let t of e)if(t.type===`childList`)for(let e of t.addedNodes)e.tagName===`LINK`&&e.rel===`modulepreload`&&n(e)}).observe(document,{childList:!0,subtree:!0});function t(e){let t={};return e.integrity&&(t.integrity=e.integrity),e.referrerPolicy&&(t.referrerPolicy=e.referrerPolicy),t.credentials=e.crossOrigin===`use-credentials`?`include`:e.crossOrigin===`anonymous`?`omit`:`same-origin`,t}function n(e){if(e.ep)return;e.ep=!0;let n=t(e);fetch(e.href,n)}})();var e=100,t=28,n=class{capacity;options;values=[];svg;line;area;constructor(n=60,r={}){this.capacity=n,this.options=r;let i=`http://www.w3.org/2000/svg`,a=r.color??`#34d399`;this.svg=document.createElementNS(i,`svg`),this.svg.setAttribute(`viewBox`,`0 0 ${e} ${t}`),this.svg.setAttribute(`preserveAspectRatio`,`none`),this.svg.setAttribute(`class`,`h-7 w-full`),this.area=document.createElementNS(i,`polygon`),this.area.setAttribute(`fill`,a),this.area.setAttribute(`fill-opacity`,`0.12`),this.line=document.createElementNS(i,`polyline`),this.line.setAttribute(`fill`,`none`),this.line.setAttribute(`stroke`,a),this.line.setAttribute(`stroke-width`,`1.5`),this.line.setAttribute(`vector-effect`,`non-scaling-stroke`),this.svg.append(this.area,this.line)}get element(){return this.svg}push(e){this.values.push(Number.isFinite(e)?e:0),this.values.length>this.capacity&&this.values.shift(),this.draw()}draw(){let n=this.values.length;if(n===0)return;let r=this.options.max??Math.max(1,...this.values),i=n>1?e/(n-1):e,a=[];for(let e=0;e<n;e++){let n=e*i,o=t-Math.min(1,Math.max(0,this.values[e]/r))*t;a.push(`${n.toFixed(2)},${o.toFixed(2)}`)}let o=(n>1?(n-1)*i:e).toFixed(2);this.line.setAttribute(`points`,a.join(` `)),this.area.setAttribute(`points`,`0,${t} ${a.join(` `)} ${o},${t}`)}},r=class{onState;handlers=new Set;socket=null;retries=0;stopped=!1;constructor(e){this.onState=e}onSnapshot(e){this.handlers.add(e)}start(){this.connect()}stop(){this.stopped=!0,this.socket?.close()}async currentSnapshot(){try{let e=await fetch(`/api/system/snapshot`);return e.ok?await e.json():null}catch{return null}}connect(){if(this.stopped)return;this.onState(this.retries===0?`connecting`:`down`);let e=location.protocol===`https:`?`wss`:`ws`,t=new WebSocket(`${e}://${location.host}/api/ws/telemetry`);this.socket=t,t.onopen=()=>{this.retries=0,this.onState(`live`)},t.onmessage=e=>{let t;try{t=JSON.parse(e.data)}catch{return}for(let e of this.handlers)e(t)},t.onclose=()=>{this.onState(`down`),this.scheduleReconnect()},t.onerror=()=>t.close()}scheduleReconnect(){if(this.stopped)return;let e=Math.min(1e3*2**this.retries,15e3);this.retries+=1,window.setTimeout(()=>this.connect(),e)}};async function i(){try{let e=await fetch(`/api/health`);if(!e.ok)return``;let t=await e.json();return t.version?`v${t.version}`:``}catch{return``}}var a=`#34d399`,o=`#38bdf8`,s=[`B`,`KiB`,`MiB`,`GiB`,`TiB`,`PiB`];function c(e){if(!Number.isFinite(e)||e<=0)return`0 B`;let t=e,n=0;for(;t>=1024&&n<s.length-1;)t/=1024,n+=1;let r=+(n>0&&t<10);return`${t.toFixed(r)} ${s[n]}`}function l(e){return`${c(e)}/s`}function u(e){if(!e)return`—`;let t=Math.floor(e/86400),n=Math.floor(e%86400/3600),r=Math.floor(e%3600/60);return t>0?`${t}d ${n}h ${r}m`:n>0?`${n}h ${r}m`:`${r}m`}function d(e){return`${e.toFixed(+(e<10))}%`}function f(e,t){let n=Math.min(100,Math.max(0,t));e.style.width=`${n}%`,e.className=n>=90?`h-full bg-rose-500`:n>=75?`h-full bg-amber-400`:`h-full bg-emerald-500`}function p(e){let t=document.querySelector(e);if(!t)throw Error(`missing element ${e}`);return t}function m(e){let t=document.createElement(`div`);return t.className=`px-3 py-3 text-slate-600`,t.textContent=e,t}var h=p(`#app`);h.innerHTML=`
<header class="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-[#1e2733] bg-[#0d131b] px-4 py-2">
  <span class="font-bold tracking-[0.25em] text-emerald-400">STACK-STACK</span>
  <span id="host" class="text-slate-300">—</span>
  <span id="uptime" class="tabular text-slate-500">up —</span>
  <span id="load" class="tabular text-slate-500">load —</span>
  <span class="ml-auto flex items-center gap-2">
    <span id="frame" class="tabular text-slate-600"></span>
    <span id="version" class="text-slate-600"></span>
    <span id="dot" class="inline-block size-2 rounded-full bg-slate-600"></span>
    <span id="link" class="text-slate-400">connecting</span>
  </span>
</header>

<main class="grid grid-cols-1 gap-3 p-3 xl:grid-cols-2 2xl:grid-cols-3">
  <section class="panel">
    <div class="panel-title flex items-baseline justify-between gap-4 border-b border-[#1e2733] px-3 py-1.5 text-[10px]">
      <span>cpu</span>
      <span id="cpu-model" class="truncate text-[10px] normal-case tracking-normal text-slate-600"></span>
    </div>
    <div class="flex items-center gap-4 px-3 py-2">
      <div id="cpu-total" class="tabular text-3xl text-slate-100">—</div>
      <div class="min-w-0 flex-1">
        <div id="cpu-spark"></div>
        <div class="tabular mt-1 flex justify-between text-[10px] text-slate-500">
          <span id="cpu-user">usr —</span>
          <span id="cpu-sys">sys —</span>
          <span id="cpu-io">io —</span>
          <span id="cpu-idle">idle —</span>
        </div>
      </div>
    </div>
    <div id="cpu-cores" class="grid grid-cols-6 gap-x-2 gap-y-1 px-3 pb-3 sm:grid-cols-8 lg:grid-cols-10"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">memory</div>
    <div class="px-3 py-2">
      <div class="flex items-baseline justify-between">
        <span id="mem-used" class="tabular text-2xl text-slate-100">—</span>
        <span id="mem-total" class="tabular text-slate-500">—</span>
      </div>
      <div class="mt-2 h-2 bg-[#1e2733]"><div id="mem-bar" class="h-full bg-emerald-500" style="width:0%"></div></div>
      <dl class="tabular mt-2 grid grid-cols-2 gap-x-4 text-[11px] text-slate-500">
        <div class="flex justify-between"><dt>cached</dt><dd id="mem-cached">—</dd></div>
        <div class="flex justify-between"><dt>buffers</dt><dd id="mem-buffers">—</dd></div>
        <div class="flex justify-between"><dt>available</dt><dd id="mem-avail">—</dd></div>
        <div class="flex justify-between"><dt>swap</dt><dd id="mem-swap">—</dd></div>
      </dl>
    </div>
  </section>

  <section class="panel xl:col-span-2 2xl:col-span-1">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">network</div>
    <div id="nets"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">filesystems</div>
    <div id="disks"></div>
  </section>

  <section class="panel">
    <div class="panel-title border-b border-[#1e2733] px-3 py-1.5 text-[10px]">smart</div>
    <div id="smart"></div>
  </section>
</main>
`;var g=p(`#host`),_=p(`#uptime`),v=p(`#load`),y=p(`#frame`),b=p(`#version`),x=p(`#dot`),S=p(`#link`),C=p(`#cpu-model`),w=p(`#cpu-total`),T=p(`#cpu-user`),E=p(`#cpu-sys`),D=p(`#cpu-io`),O=p(`#cpu-idle`),k=p(`#cpu-cores`),A=p(`#mem-used`),j=p(`#mem-total`),ee=p(`#mem-bar`),M=p(`#mem-cached`),N=p(`#mem-buffers`),P=p(`#mem-avail`),F=p(`#mem-swap`),I=p(`#nets`),L=p(`#disks`),R=p(`#smart`),z=new n(120,{max:100,color:a});p(`#cpu-spark`).append(z.element);var B=[],V=-1;function H(e){if(e.length!==V){V=e.length,B.length=0,k.replaceChildren();for(let t=0;t<e.length;t++){let e=document.createElement(`div`),t=document.createElement(`div`);t.className=`tabular text-[9px] text-slate-600`;let n=document.createElement(`div`);n.className=`h-1.5 bg-[#1e2733]`;let r=document.createElement(`div`);r.className=`h-full bg-emerald-500`,r.style.width=`0%`,n.append(r),e.append(t,n),k.append(e),B.push({bar:r,label:t})}}e.forEach((e,t)=>{let n=B[t];n&&(f(n.bar,e),n.label.textContent=`${t}·${e.toFixed(0)}`)})}var U=new Map,W=``;function G(e){let t=e.map(e=>e.iface).sort().join(`,`);if(t!==W){W=t,U.clear(),I.replaceChildren(),e.length===0&&I.append(m(`no interfaces reported by /proc/net/dev`));for(let t of e){let e=document.createElement(`div`);e.className=`border-b border-[#161d27] px-3 py-2 last:border-b-0`;let r=document.createElement(`div`);r.className=`flex items-baseline justify-between`;let i=document.createElement(`span`);i.className=`text-slate-300`,i.textContent=t.iface;let s=document.createElement(`span`);s.className=`tabular text-[10px] text-slate-600`,r.append(i,s);let c=document.createElement(`div`);c.className=`mt-1 grid grid-cols-2 gap-3`;let l=document.createElement(`div`);l.className=`tabular text-[11px] text-slate-500`;let u=document.createElement(`span`);u.className=`text-emerald-400`,l.append(`↓ `,u);let d=new n(60,{color:a}),f=document.createElement(`div`);f.className=`tabular text-[11px] text-slate-500`;let p=document.createElement(`span`);p.className=`text-sky-400`,f.append(`↑ `,p);let m=new n(60,{color:o}),h=document.createElement(`div`);h.append(l,d.element);let g=document.createElement(`div`);g.append(f,m.element),c.append(h,g),e.append(r,c),I.append(e),U.set(t.iface,{rx:u,tx:p,total:s,rxSpark:d,txSpark:m})}}for(let t of e){let e=U.get(t.iface);e&&(e.rx.textContent=l(t.rx_bps),e.tx.textContent=l(t.tx_bps),e.total.textContent=`Σ ↓${c(t.rx_bytes)} ↑${c(t.tx_bytes)}`,e.rxSpark.push(t.rx_bps),e.txSpark.push(t.tx_bps))}}var K=new Map,q=``;function J(e){let t=e.map(e=>e.mount).sort().join(`,`);if(t!==q){q=t,K.clear(),L.replaceChildren(),e.length===0&&L.append(m(`no real filesystems mounted`));for(let t of e){let e=document.createElement(`div`);e.className=`border-b border-[#161d27] px-3 py-2 last:border-b-0`;let n=document.createElement(`div`);n.className=`flex items-baseline justify-between gap-3`;let r=document.createElement(`span`);r.className=`truncate text-slate-300`,r.textContent=t.mount;let i=document.createElement(`span`);i.className=`tabular shrink-0 text-[11px] text-slate-500`,n.append(r,i);let a=document.createElement(`div`);a.className=`mt-1.5 h-1.5 bg-[#1e2733]`;let o=document.createElement(`div`);o.className=`h-full bg-emerald-500`,o.style.width=`0%`,a.append(o);let s=document.createElement(`div`);s.className=`tabular mt-1 text-[10px] text-slate-600`,s.textContent=`${t.device} · ${t.fs}`,e.append(n,a,s),L.append(e),K.set(t.mount,{bar:o,usage:i})}}for(let t of e){let e=K.get(t.mount);e&&(f(e.bar,t.used_pct),e.usage.textContent=`${c(t.used_bytes)} / ${c(t.total_bytes)} · ${d(t.used_pct)}`)}}var Y=new Map,X=``;function Z(e){let t=e.map(e=>e.device).sort().join(`,`);if(t!==X){X=t,Y.clear(),R.replaceChildren(),e.length===0&&R.append(m(`no SMART devices (smartctl missing or no disks)`));for(let t of e){let e=document.createElement(`div`);e.className=`flex items-baseline justify-between gap-3 border-b border-[#161d27] px-3 py-2 last:border-b-0`;let n=document.createElement(`div`);n.className=`min-w-0`;let r=document.createElement(`div`);r.className=`truncate text-slate-300`,r.textContent=t.device;let i=document.createElement(`div`);i.className=`truncate text-[10px] text-slate-600`,i.textContent=t.model??``,n.append(r,i);let a=document.createElement(`div`);a.className=`tabular flex shrink-0 items-baseline gap-3 text-[11px]`;let o=document.createElement(`span`),s=document.createElement(`span`);s.className=`text-slate-500`;let c=document.createElement(`span`);c.className=`text-slate-600`,a.append(o,s,c),e.append(n,a),R.append(e),Y.set(t.device,{health:o,temp:s,hours:c})}}for(let t of e){let e=Y.get(t.device);e&&(t.healthy===null?(e.health.textContent=`?`,e.health.className=`text-slate-600`):(e.health.textContent=t.healthy?`OK`:`FAIL`,e.health.className=t.healthy?`text-emerald-400`:`text-rose-500`),e.temp.textContent=t.temperature_c===null?``:`${t.temperature_c}°C`,e.hours.textContent=t.power_on_hours===null?``:`${t.power_on_hours}h`)}}function Q(e){document.title=`${e.hostname} · stack-stack`,g.textContent=e.hostname,_.textContent=`up ${u(e.uptime_s)}`,v.textContent=`load ${e.load.map(e=>e.toFixed(2)).join(` `)}`,y.textContent=new Date(e.ts_ms).toLocaleTimeString(),w.textContent=d(e.cpu.usage_pct),z.push(e.cpu.usage_pct),C.textContent=e.cpu.model??``,T.textContent=`usr ${e.cpu.user_pct.toFixed(0)}`,E.textContent=`sys ${e.cpu.system_pct.toFixed(0)}`,D.textContent=`io ${e.cpu.iowait_pct.toFixed(0)}`,O.textContent=`idle ${e.cpu.idle_pct.toFixed(0)}`,H(e.cpu.per_core);let t=e.memory;A.textContent=c(t.used_bytes),j.textContent=`of ${c(t.total_bytes)} · ${d(t.used_pct)}`,f(ee,t.used_pct),M.textContent=c(t.cached_bytes),N.textContent=c(t.buffers_bytes),P.textContent=c(t.available_bytes),F.textContent=t.swap_total_bytes===0?`none`:`${c(t.swap_used_bytes)} / ${c(t.swap_total_bytes)}`,G(e.networks),J(e.disks),Z(e.smart)}function te(e){S.textContent=e,x.className=e===`live`?`inline-block size-2 rounded-full bg-emerald-400`:e===`connecting`?`inline-block size-2 rounded-full bg-amber-400`:`inline-block size-2 rounded-full bg-rose-500`}var $=new r(te);$.onSnapshot(Q),i().then(e=>{b.textContent=e}),$.currentSnapshot().then(e=>{e&&Q(e)}),$.start();