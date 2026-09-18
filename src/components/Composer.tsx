import { useRef } from "react";
import { ModelSelect } from "./ModelSelect";
import { EXAMPLE_TASKS, type ModelId, type RunState } from "../data/mock";
export function Composer({task,setTask,model,setModel,state,onRun}:{task:string;setTask:(t:string)=>void;model:ModelId;setModel:(m:ModelId)=>void;state:RunState;onRun:()=>void}) {
 const busy=state==="working"||state==="verifying"; const canRun=task.trim().length>0&&!busy; const areaRef=useRef<HTMLTextAreaElement>(null);
 return <section aria-label="New task" className="task-well">
   <div className="mb-5"><p className="eyebrow">New task</p><h1 className="mt-2 text-[28px] font-medium leading-[1.15] tracking-[-0.035em] text-text sm:text-[34px]">What should REX do?</h1></div>
   <textarea id="task-input" ref={areaRef} rows={3} value={task} disabled={busy} onChange={e=>setTask(e.target.value)} onKeyDown={e=>{if((e.metaKey||e.ctrlKey)&&e.key==="Enter"&&canRun)onRun()}} aria-label="Task description" placeholder="Describe one task in plain language" className="task-input" />
   <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
    <ModelSelect value={model} onChange={setModel}/>
    <div className="flex items-center gap-3"><span className="hidden text-xs text-faint sm:inline">Ctrl + Enter</span><button type="button" onClick={onRun} disabled={!canRun} title={!task.trim()?"Describe a task first":busy?"A run is in progress":"Start the run"} className="run-button"><span>{busy?"Running":"Run task"}</span><svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 8h9m-3.5-3.5L12.5 8 9 11.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/></svg></button></div>
   </div>
   {state==="idle"&&task.trim()===""&&<div className="mt-5 border-t border-line pt-4" aria-label="Example tasks"><p className="mb-2 text-xs text-faint">Try an example</p><div className="flex flex-col items-start gap-1">{EXAMPLE_TASKS.slice(0,2).map(t=><button key={t} type="button" onClick={()=>{setTask(t);areaRef.current?.focus()}} className="example-link">{t}</button>)}</div></div>}
 </section>
}
