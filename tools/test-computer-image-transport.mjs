// Real captured native PNG through the production Pi SDK; provider replies are synthetic.
import fs from 'node:fs';
import assert from 'node:assert/strict';
import {runSession} from '../harness/pi/session.mjs';
const png=fs.readFileSync(process.env.KINDRED_EXECUTOR_IMAGE_PATH);
assert.equal(png.readUInt32BE(16),1440);assert.equal(png.readUInt32BE(20),900);
const image='data:image/png;base64,'+png.toString('base64');
const tool={name:'computer_click',description:'Fixture input',inputSchema:{type:'object',properties:{x:{type:'integer'},y:{type:'integer'}},required:['x','y'],additionalProperties:false}};
function sse(delta,finish){const c=d=>({id:'fixture',object:'chat.completion.chunk',created:1,model:'fixture-model',choices:[{index:0,delta:d,finish_reason:null}]});const end=c({});end.choices[0].finish_reason=finish;return new Response('data: '+JSON.stringify(c(delta))+'\n\ndata: '+JSON.stringify(end)+'\n\ndata: [DONE]\n\n',{headers:{'Content-Type':'text/event-stream'}});}
for(const provider of ['openrouter','custom-11111111-1111-4111-8111-111111111111']){
let requests=0,calls=0;
const result=await runSession({protocol:1,provider,model:'fixture-model',model_info:{id:'fixture-model',vision:true,context_window:128000,max_tokens:4096},endpoint:provider==='openrouter'?undefined:'http://127.0.0.1:8000/v1/chat/completions',api_key:'fixture-key',instructions:'Fixture',prompt:'Inspect result',tools:[tool],max_steps:2,reasoning_effort:'',require_zdr:true},async(id,name,args)=>{calls++;assert.equal(id,'input-1');assert.equal(name,'computer_click');assert.deepEqual(args,{x:900,y:450});return {text:'Input applied; current native image1440x900',image,action_applied:true,observation_after_action:true};},()=>{},undefined,{fetch:async req=>{
const body=await req.json();assert.equal(body.model,'fixture-model');requests++;
if(requests===1)return sse({role:'assistant',tool_calls:[{index:0,id:'input-1',type:'function',function:{name:'computer_click',arguments:'{"x":900,"y":450}'}}]},'tool_calls');
const encoded=JSON.stringify(body.messages);assert(encoded.includes(image),'native image preserved without resize/re-encoding');assert(encoded.includes('input-1'));return sse({role:'assistant',content:'Fixture image received'},'stop');}});
assert.equal(result.output,'Fixture image received');assert.equal(calls,1);assert.equal(requests,2);console.log(JSON.stringify({provider,requests,input_calls:calls,png_bytes:png.length,dimensions:[1440,900],unaltered:true}));}
