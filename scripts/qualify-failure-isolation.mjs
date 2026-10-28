// Controlled loopback regression for item and collection failure isolation.
// Uses the packaged host and plugins. No external server is contacted.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { Client, successful } from "./test-client.mjs";
import { wire, metadata } from "./synthetic-user-codes.mjs";
const [executable, plugins, evidencePath] = process.argv.slice(2);
assert.ok(executable && plugins && evidencePath, "Supply executable, plugins and evidence paths.");
const root = await mkdtemp(join(tmpdir(), "herbert-isolation-"));
const company = wire.returnObj.UDCodeType[0].Company;
const good = wire.returnObj.UDCodeType[0].CodeTypeID;
const identity = (id) => ({category:"C01",kind:"UDCodeType",scope:{type:"company",company},keys:{CodeTypeID:id}});
const later = {category:"C02",kind:"UDMapping",scope:{type:"company",company},keys:{MapTableID:"7"}};
const checks = [], errors = [], requests = [];
let scenario = "refusal", client;
const send = (response, status, value) => { response.writeHead(status, {"content-type":"application/json"}); response.end(JSON.stringify(value)); };
const server = createServer(async (request, response) => {
  try {
    let body = ""; for await (const chunk of request) body += chunk;
    const args = JSON.parse(body || "{}");
    const parts = new URL(request.url, "http://127.0.0.1").pathname.split("/");
    const method = parts.at(-1), service = parts.at(-2);
    requests.push({scenario,service,method,args});
    if (service === "Ice.Lib.SessionModSvc" && method === "GetDatabaseVersion") return send(response,200,{returnObj:"fixture-5.2.100"});
    if (service === "Ice.BO.UserFileSvc" && ["GetUserFile","GetByID"].includes(method)) return send(response,200,{returnObj:{UserFile:[{UserID:"fixture-user",DcdUserID:"fixture-user",SecurityMgr:true,UserDisabled:false,LockedOut:false}]}});
    if (service === "Ice.BO.BAQDesignerSvc" && method === "GetFieldList") {
      const entry = metadata.find(entry => entry.table === args.dataTableID);
      assert.ok(entry, "Unexpected metadata table"); return send(response,200,entry.wire);
    }
    if (service === "Ice.BO.DynamicQuerySvc" && method === "Execute") return send(response,200,{returnObj:{Results:wire.returnObj.UDCodes,Errors:[]}});
    if (service === "Ice.BO.UserCodesSvc" && method === "GetList") {
      if (scenario === "discovery-recovered") return send(response,200,{returnObj:{UDCodeTypeList:wire.returnObj.UDCodeType},parameters:{morePages:false}});
      if (scenario === "discovery-malformed") return send(response,200,{returnObj:{},parameters:{morePages:false}});
      if (scenario === "discovery-later-refusal") {
        if (args.absolutePage === 1) return send(response,200,{returnObj:{UDCodeTypeList:wire.returnObj.UDCodeType},parameters:{morePages:true}});
        assert.equal(args.absolutePage,2);
        return send(response,403,{ErrorType:"Ice.Common.SecurityAccessException",ErrorMessage:"Synthetic later-page refusal."});
      }
      throw new Error("Unexpected inventory scenario");
    }
    if (service === "Ice.BO.UserCodesSvc" && method === "GetRows") {
      if (args.whereClauseUDCodeType.includes("'REFUSED'")) {
        if (scenario === "refusal") return send(response,403,{ErrorType:"Ice.Common.SecurityAccessException",ErrorMessage:"Synthetic item refusal."});
        return send(response,200,{returnObj:{UDCodeType:[{Company:company,CodeTypeID:"REFUSED",SystemFlag:false}]},parameters:{morePages:false}});
      }
      assert.equal(args.whereClauseUDCodeType,"CodeTypeID = '" + good + "' AND SystemFlag = false");
      return send(response,200,wire);
    }
    if (service === "Ice.BO.UDMapSvc" && method === "GetByID") {
      assert.equal(args.mapTableID,7);
      return send(response,200,{returnObj:{UDMapTable:[{Company:company,MapTableID:7,SystemFlag:false}],UDMapColumn:[]},parameters:{morePages:false}});
    }
    throw new Error("Unexpected fixture route: " + service + "." + method);
  } catch (error) { errors.push(String(error)); send(response,500,{ErrorType:"FixtureContractError",ErrorMessage:String(error)}); }
});
server.listen(0,"127.0.0.1"); await once(server,"listening");
try {
  for (const mode of ["cli","mcp"]) {
    client = new Client(executable,mode,join(root,mode+"-state"),plugins,join(root,mode+"-coordination"));
    await client.initialize();
    const settings = successful(await client.call("settings.inspect")).effective;
    // Use the lowest supported request budget. Check that all independent work finishes.
    successful(await client.call("settings.update",{...settings,concurrency:2}));
    successful(await client.call("connection.connect",{url:"http://127.0.0.1:"+server.address().port,company,authMode:"basicApiKey",credentialSource:{type:"inline",credentials:{username:"fixture-user",password:"synthetic-password",apiKey:"fixture-key"}}}));
    for (scenario of ["refusal","partial"]) {
      const start = requests.length;
      const run = successful(await client.call("retrieve.start",{requestId:randomUUID(),destination:join(root,mode+"-"+scenario),outputMode:"separate",identities:[identity("REFUSED"),identity(good),later],temporaryAccess:false}));
      let status;
      do { status = successful(await client.call("run.status",{id:run.runId})); if (status.executionActive) await new Promise(resolve=>setTimeout(resolve,10)); } while (status.executionActive);
      assert.equal(status.phase,"partial",JSON.stringify(status.diagnostics));
      assert.equal(status.artifacts.length,3);
      const first = status.artifacts.find(item=>item.identity.keys.CodeTypeID==="REFUSED");
      const sibling = status.artifacts.find(item=>item.identity.keys.CodeTypeID===good);
      const next = status.artifacts.find(item=>item.identity.category==="C02");
      assert.equal(first.acquisition,scenario === "refusal" ? "inaccessible" : "partial");
      assert.notEqual(first.processing,"derived");
      for (const item of [sibling,next]) { assert.equal(item.acquisition,"collected",JSON.stringify(item)); assert.equal(item.processing,"derived"); assert.ok(item.files.length>0); }
      const calls = requests.slice(start);
      const refusedIndex = calls.findIndex(call=>call.service==="Ice.BO.UserCodesSvc" && call.args.whereClauseUDCodeType?.includes("'REFUSED'"));
      const siblingIndex = calls.findIndex(call=>call.service==="Ice.BO.UserCodesSvc" && call.args.whereClauseUDCodeType?.includes("'"+good+"'"));
      const laterIndex = calls.findIndex(call=>call.service==="Ice.BO.UDMapSvc");
      assert.ok(refusedIndex>=0 && siblingIndex>=0 && laterIndex>refusedIndex);
      assert.deepEqual(errors,[]);
      checks.push({mode,scenario,passed:true,outcomes:status.artifacts.map(item=>({identity:item.identity,acquisition:item.acquisition,processing:item.processing})),requestOrder:[refusedIndex,siblingIndex,laterIndex]});
    }
    for (scenario of ["discovery-malformed","discovery-later-refusal"]) {
      const run = successful(await client.call("backup.start",{requestId:randomUUID(),destination:join(root,mode+"-"+scenario),outputMode:"separate",categories:["C01"],temporaryAccess:false}));
      let status;
      do { status = successful(await client.call("run.status",{id:run.runId})); if (status.executionActive) await new Promise(resolve=>setTimeout(resolve,10)); } while (status.executionActive);
      assert.equal(status.phase,"partial",JSON.stringify(status));
      assert.equal(status.categoryOutcomes.C01,"discoveryFailed");
      assert.notEqual(status.categoryOutcomes.C01,"verifiedAbsent");
      assert.equal(status.artifacts.length,scenario === "discovery-later-refusal" ? 1 : 0);
      if (status.artifacts.length) {
        assert.equal(status.artifacts[0].acquisition,"collected");
        assert.equal(status.artifacts[0].processing,"derived");
        assert.equal(status.artifacts[0].identity.keys.CodeTypeID,good);
      }
      assert.deepEqual(errors,[]);
      checks.push({mode,scenario,passed:true,outcome:status.categoryOutcomes.C01,retainedArtifacts:status.artifacts.length});
      const failedScenario = scenario;
      scenario = "discovery-recovered";
      successful(await client.call("run.retryFailed",{id:run.runId}));
      do { status = successful(await client.call("run.status",{id:run.runId})); if (status.executionActive) await new Promise(resolve=>setTimeout(resolve,10)); } while (status.executionActive);
      assert.equal(status.phase,"completed",JSON.stringify(status));
      assert.equal(status.categoryOutcomes.C01,"discovered");
      assert.equal(status.artifacts.length,1);
      assert.equal(status.artifacts[0].acquisition,"collected");
      assert.equal(status.artifacts[0].processing,"derived");
      assert.deepEqual(errors,[]);
      checks.push({mode,scenario:"retry-"+failedScenario,passed:true,outcome:status.categoryOutcomes.C01,retainedArtifacts:status.artifacts.length});

    }
    successful(await client.call("connection.disconnect")); await client.close(); client=null;
  }
  await writeFile(evidencePath,JSON.stringify({passed:true,proofClass:"Packaged CLI and MCP against synthetic loopback",checks,nativeMutations:false},null,2));
  console.log(JSON.stringify({passed:true,checks:checks.length}));
} catch (error) {
  await writeFile(evidencePath,JSON.stringify({passed:false,error:String(error),checks,errors,requests},null,2)); throw error;
} finally {
  if (client) await client.close().catch(()=>{});
  server.closeAllConnections(); await new Promise(resolve=>server.close(resolve));
  assert.equal(dirname(resolve(root)),resolve(tmpdir())); await rm(root,{recursive:true,force:true});
}
