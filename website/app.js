const root=document.documentElement, stage=document.querySelector('#stage');
addEventListener('pointermove',e=>{root.style.setProperty('--px',e.clientX/innerWidth);if(stage){const r=stage.getBoundingClientRect();stage.style.setProperty('--mx',`${((e.clientX-r.left)/r.width)*100}%`);stage.style.setProperty('--my',`${((e.clientY-r.top)/r.height)*100}%`)}});
const reveal=new IntersectionObserver(es=>es.forEach(e=>{if(e.isIntersecting)e.target.classList.add('in')}),{threshold:.15});document.querySelectorAll('section,article').forEach(x=>reveal.observe(x));
addEventListener('scroll',()=>{document.body.style.setProperty('--scroll',scrollY/document.body.scrollHeight)});
