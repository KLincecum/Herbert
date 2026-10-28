// Exercise the packaged CLI and MCP with owned local state and disposable credentials.
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,readFile,writeFile,rm} from 'node:fs/promises';
import {join,resolve,dirname} from 'node:path';
import {randomUUID} from 'node:crypto';
import {Client,successful} from './test-client.mjs';
const [folder,evidence]=process.argv.slice(2);
assert.ok(folder && evidence);
await mkdir(evidence,{recursive:true});
const root=await mkdtemp(join(resolve(evidence),'host-parity-fixture-'));
const secret='owned-fixture-'+randomUUID();
const manifest=JSON.parse(await readFile(join(folder,"package.json"),"utf8"));
const executable=join(folder,manifest.files.find(file=>/^herbert(?:\.exe)?$/.test(file.path)).path);
const checks=[];
async function check(mode,name,body){try{await body();checks.push({mode,name,passed:true});}catch(e){checks.push({mode,name,passed:false,error:String(e.message).replaceAll(secret,'[redacted]')});}await writeFile(join(evidence,'host-parity.json'),JSON.stringify({complete:false,checks},null,2));}
try{
for(const mode of ['cli','mcp']){
 const client=new Client(executable,mode,join(root,mode),join(folder,'plugins'));
 const id='host-parity-'+randomUUID();
 const call=async(method,args={})=>{const reply=await client.call(method,args);assert.ok(!JSON.stringify(reply).includes(secret),'A reply exposed a supplied secret');return reply;};
 try{
 await client.initialize();
 const schema=successful(await call('schema'));
 await check(mode,'operation schemas agree with MCP tools',async()=>{
  assert.ok(schema.operations.length>0);
  if(mode==='mcp'){const list=await client.request({jsonrpc:'2.0',id:++client.sequence,method:'tools/list',params:{}});assert.deepEqual(list.result.tools.map(t=>[t.name,t.inputSchema]),schema.operations.map(o=>[o.name.replaceAll('.','_'),o.inputSchema]));}
 });
 for(const operation of schema.operations.filter(o=>o.inputSchema.additionalProperties===false && Object.keys(o.inputSchema.properties??{}).length===0)){
  await check(mode,operation.name+' rejects unsupported arguments',async()=>{const r=await call(operation.name,{unexpected:'owned-fixture'});assert.equal(r.ok,false);assert.equal(r.error.code,'INVALID_INPUT');});
 }
 await check(mode,'profile save normalizes browser URL and preserves exact IDs',async()=>{successful(await call('profiles.upsert',{id,name:'Owned Ω environment',url:'https://EXAMPLE.invalid/ExactCase/APPS/erp/Home?company=X#screen',authMode:'session',defaults:{company:'Fixture'}}));const p=successful(await call('profiles.list')).find(p=>p.id===id);assert.equal(p.url,'https://example.invalid/ExactCase/');assert.equal(p.name,'Owned Ω environment');successful(await call('profiles.rename',{id,name:'Renamed environment'}));assert.equal(successful(await call('profiles.list')).find(p=>p.id===id).name,'Renamed environment');});
 await check(mode,'bad profile leaves existing metadata unchanged',async()=>{const before=successful(await call('profiles.list'));const r=await call('profiles.upsert',{id,url:'https://user:'+secret+'@example.invalid',defaults:{}});assert.equal(r.ok,false);assert.deepEqual(successful(await call('profiles.list')),before);});
 await check(mode,'settings apply and invalid settings preserve prior values',async()=>{const defaults=successful(await call('settings.inspect')).effective;successful(await call('settings.update',{...defaults,concurrency:6,pageSize:31}));const before=successful(await call('settings.inspect')).effective;assert.equal(before.concurrency,6);const r=await call('settings.update',{...before,concurrency:0});assert.equal(r.ok,false);assert.deepEqual(successful(await call('settings.inspect')).effective,before);successful(await call('settings.update',defaults));});
 await check(mode,'credential save replace forget and profile preservation',async()=>{
 for(const password of [secret,secret+'-replacement']){successful(await call('credentials.save',{id,credentials:{username:'owned-test',password,apiKey:null}}));assert.equal(successful(await call('credentials.status',{id})).status,'saved');}
 successful(await call('credentials.forget',{id}));assert.notEqual(successful(await call('credentials.status',{id})).status,'saved');assert.ok(successful(await call('profiles.list')).some(p=>p.id===id));
 });
 await check(mode,'invalid native operations fail without a connection',async()=>{for(const method of ['companies.list','retrieve.start','backup.start']){const r=await call(method,method==='companies.list'?{}:{requestId:randomUUID(),destination:join(root,'unused'),outputMode:'separate',categories:['C01']});assert.equal(r.ok,false);}});
 await check(mode,'missing run fails without inventing state',async()=>{const before=successful(await call('run.list'));for(const method of ['run.status','run.pause','run.resume','run.cancel','run.recover','run.retryFailed','run.reprocess']){assert.equal((await call(method,{id:'missing-'+randomUUID()})).ok,false);}assert.deepEqual(successful(await call('run.list')),before);});
 await check(mode,'profile deletion survives process restart',async()=>{successful(await call('profiles.remove',{id}));assert.ok(!successful(await call('profiles.list')).some(p=>p.id===id));});
 } finally {await client.call('credentials.forget',{id}).catch(()=>{});await client.call('profiles.remove',{id}).catch(()=>{});await client.close();}
 const restarted=new Client(executable,mode,join(root,mode),join(folder,'plugins'));try{await restarted.initialize();await check(mode,'restart preserves profile removal',async()=>{assert.ok(!successful(await restarted.call('profiles.list')).some(p=>p.id===id));});}finally{await restarted.close();}
}
}finally{assert.equal(dirname(root),resolve(evidence));await rm(root,{recursive:true,force:true});}
const result={complete:true,passed:checks.every(c=>c.passed),checks,fixtureCleanup:true,nativeCalls:0};
await writeFile(join(evidence,'host-parity.json'),JSON.stringify(result,null,2));
console.log(JSON.stringify({passed:result.passed,checks:checks.length,failures:checks.filter(c=>!c.passed)}));
if(!result.passed)process.exitCode=1;
