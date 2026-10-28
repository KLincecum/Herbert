// Exercise the real CLI and MCP against an synthetic loopback fixture.
// The fixture can commit a membership before losing the response. These checks
// prove ownership and cleanup behavior without changing a real Epicor account.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import {
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { Client, successful } from "./test-client.mjs";

import { wire, metadata } from "./synthetic-user-codes.mjs";
const [executable, plugins, evidencePath] = process.argv.slice(2);
assert.ok(executable && plugins && evidencePath,
  "Supply executable, plugins and evidence paths.");
const codeType = wire.returnObj.UDCodeType[0].CodeTypeID;
const company = wire.returnObj.UDCodeType[0].Company;
const identity = {
  category: "C01",
  kind: "UDCodeType",
  scope: { type: "company", company },
  keys: { CodeTypeID: codeType },
};
const root = await mkdtemp(join(tmpdir(), "herbert-access-"));
const checks = [];
const clients = [];
const secret = randomUUID();
const model = {
  groups: ["BASE"],
  revision: 1,
  assignments: 0,
  removals: 0,
  mode: "normal",
  allowedReads: 0,
  holds: new Map(),
  holdReads: false,
  state: "",
  coordination: "",
  intentChecked: false,
  entryList: "blocked_test,readers_test",
  noEntryList: "BLOCKED_TEST,OTHER_DENIED",
};
const rowId = randomUUID();
const groupId = randomUUID();
function account() {
  return {
    UserID: "fixture-user",
    SecurityMgr: true,
    UserDisabled: false,
    LockedOut: false,
    GroupList: model.groups.join("~"),
    SysRowID: rowId,
    SysRevID: model.revision,
    PwdChangeRequestOn: "2026-01-01T00:00:00",
    PwdCreateDate: "2026-01-01T00:00:00",
    PwdLastChanged: "2026-01-01T00:00:00",
    PasswordNeverExpires: true,
  };
}
function send(response, status, value) {
  response.writeHead(status, { "content-type": "application/json" });
  response.end(JSON.stringify(value));
}
async function registry() {
  const directory = join(model.coordination, "access");
  const names = (await readdir(directory)).filter((name) =>
    name.endsWith(".json"),
  );
  assert.equal(names.length, 1);
  return JSON.parse(await readFile(join(directory, names[0]), "utf8"));
}
const server = createServer(async (request, response) => {
  try {
    let body = "";
    for await (const chunk of request) body += chunk;
    const args = JSON.parse(body || "{}");
    const parts = new URL(request.url, "http://127.0.0.1").pathname.split("/");
    const method = parts.at(-1);
    const service = parts.at(-2);
    assert.equal(request.headers["x-api-key"], "fixture-key");
    if (service === "Ice.Lib.SessionModSvc" && method === "GetDatabaseVersion")
      return send(response, 200, { returnObj: "fixture-5.1.100.29" });
    if (
      service === "Ice.BO.UserFileSvc" &&
      ["GetUserFile", "GetByID"].includes(method)
    )
      return send(response, 200, { returnObj: { UserFile: [account()] } });
    if (service === "Ice.BO.SecGroupSvc" && method === "GetByID") {
      const id = args.secGroupCode.toUpperCase();
      assert.ok(["BASE", "READERS_TEST", "LATER", "BLOCKED_TEST", "OTHER_DENIED"].includes(id));
      return send(response, 200, {
        returnObj: {
          SecGroup: [
            {
              Company: company,
              SecGroupCode: id,
              SysRowID: groupId,
              SysRevID: 1,
            },
          ],
        },
      });
    }
    if (service === "Ice.BO.ObjectSecuritySvc" && method === "GetByIDEx") {
      assert.equal(args.secCode, "bo.UserCodes");
      const policy = {
        Company: company,
        SecCode: "bo.UserCodes.GetRows",
        ParentSecCode: "bo.UserCodes",
        EntryList: model.entryList,
        NoEntryList: model.noEntryList,
        SecurityMgr: false,
        AllowAll: false,
        DisallowAll: false,
        SysRowID: groupId,
        SysRevID: 1,
      };
      return send(response, 200, {
        returnObj: { Security: [], ObjectSecurityMethod: [policy] },
      });
    }
    if (service === "Ice.BO.UserFileSvc" && method === "AssignGroupToUsers") {
      assert.equal(args.userIdList, "fixture-user");
      assert.equal(args.groupId, "READERS_TEST");
      const saved = await registry();
      const member = saved.memberships.READERS_TEST;
      assert.equal(member.phase, "grantPending");
      assert.equal(member.accountRowId, rowId);
      assert.equal(member.nativeWriteConfirmed, false);
      model.intentChecked = true;
      model.assignments++;
      model.groups.push(args.groupId);
      model.revision++;
      if (model.mode === "lostGrantResponse") return request.socket.destroy();
      return send(response, 200, { returnObj: {} });
    }
    if (service === "Ice.BO.UserFileSvc" && method === "UpdateExt") {
      const rows = args.ds.UserFile;
      assert.equal(rows.length, 1);
      const row = rows[0];
      assert.equal(row.UserID, "fixture-user");
      assert.equal(row.SysRowID, rowId);
      assert.equal(row.SysRevID, model.revision);
      assert.equal(row.PwdChangeRequestOn, account().PwdChangeRequestOn);
      assert.equal(row.RowMod, "U");
      assert.deepEqual(
        Object.keys(row).sort(),
        [
          "GroupList",
          "PwdChangeRequestOn",
          "RowMod",
          "SysRevID",
          "SysRowID",
          "UserID",
        ].sort(),
      );
      model.groups = row.GroupList.split("~").filter(Boolean);
      model.revision++;
      model.removals++;
      return send(response, 200, { returnObj: { BOUpdError: [] } });
    }
    if (service === "Ice.BO.BAQDesignerSvc" && method === "GetFieldList") {
      const target = metadata.find((entry) => entry.table === args.dataTableID);
      assert.ok(target);
      return send(response, 200, target.wire);
    }
    if (service === "Ice.BO.DynamicQuerySvc" && method === "Execute") {
      return send(response, 200, {
        returnObj: { Results: wire.returnObj.UDCodes, Errors: [] },
      });
    }
    if (service === "Ice.BO.UserCodesSvc" && method === "GetRows") {
      assert.equal(args.whereClauseUDCodeType, "CodeTypeID = '" + codeType.replaceAll("'", "''") + "' AND SystemFlag = false");
      assert.equal(args.whereClauseUDCodes, "SystemFlag = false");
      const allowed = model.entryList.split(",").map(id => id.trim().toUpperCase());
      const denied = model.noEntryList.split(",").map(id => id.trim().toUpperCase());
      if (!model.groups.some(id => allowed.includes(id)) || model.groups.some(id => denied.includes(id)))
        return send(response, 403, {
          ErrorType: "Ice.Common.SecurityAccessException",
          ErrorMessage: "The fixture requires its declared group.",
        });
      const number = ++model.allowedReads;
      if (model.holdReads)
        model.holds.set(number, () => send(response, 200, wire));
      else send(response, 200, wire);
      return;
    }
    throw new Error("Unexpected fixture route: " + service + "." + method);
  } catch (error) {
    process.stderr.write("Fixture contract failure: " + String(error) + "\n");
    send(response, 500, {
      ErrorType: "FixtureContractError",
      ErrorMessage: String(error),
    });
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const url = "http://127.0.0.1:" + server.address().port;
async function client(mode, state) {
  const c = new Client(executable, mode, state, plugins, model.coordination);
  clients.push(c);
  await c.initialize();
  successful(
    await c.call("connection.connect", {
      url,
      company,
      authMode: "basicApiKey",
      credentialSource: {
        type: "inline",
        credentials: {
          username: "fixture-user",
          password: secret,
          apiKey: "fixture-key",
        },
      },
    }),
  );
  return c;
}
async function waitUntil(predicate) {
  while (!(await predicate()))
    await new Promise((resolve) => setTimeout(resolve, 10));
}
async function finished(c, id) {
  let journal;
  await waitUntil(async () => {
    journal = successful(await c.call("run.status", { id }));
    return [
      "completed",
      "partial",
      "failed",
      "canceled",
      "destinationReleaseUncertain",
    ].includes(journal.phase) && journal.executionActive !== true;
  });
  return journal;
}
async function settled(c, method, args = {}) {
  while (true) {
    const reply = await c.call(method, args);
    if (reply.ok) return reply.result;
    assert.equal(reply.error.code, "RUN_ACTIVE");
    // A terminal journal can precede release of the process-owned execution lease.
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}
async function start(c, label, temporaryAccess) {
  return successful(
    await c.call("retrieve.start", {
      requestId: randomUUID(),
      destination: join(root, label),
      outputMode: "separate",
      identities: [identity],
      temporaryAccess,
    }),
  );
}
try {
 model.entryList='BASE';model.noEntryList='OTHER_DENIED';
 for(const mode of ['cli','mcp']){
  model.state=join(root,mode+'-lifecycle');model.coordination=join(root,mode+'-coordination');
  model.holdReads=true;model.holds.clear();model.allowedReads=0;
  const first=await client(mode,model.state);
  const args={requestId:randomUUID(),destination:join(root,mode+'-destination'),outputMode:'refresh',identities:[identity],temporaryAccess:false};
  const started=successful(await first.call('retrieve.start',args));
  await waitUntil(()=>model.holds.has(1));
  const duplicate=successful(await first.call('retrieve.start',args));assert.equal(duplicate.runId,started.runId);assert.equal(model.allowedReads,1);
  checks.push(mode+': repeated request attaches to the active run without another native read.');
  assert.equal(successful(await first.call('run.pause',{id:started.runId})).accepted,true);
  assert.equal(successful(await first.call('run.status',{id:started.runId})).pauseRequested,true);
  assert.equal(successful(await first.call('run.resume',{id:started.runId})).accepted,true);
  assert.equal(successful(await first.call('run.status',{id:started.runId})).pauseRequested,false);
  const other=await client(mode==='cli'?'mcp':'cli',join(root,mode+'-other-state'));
  const conflict=await other.call('retrieve.start',{...args,requestId:randomUUID()});assert.equal(conflict.ok,false);assert.equal(conflict.error.code,'DESTINATION_BUSY');
  checks.push(mode+': a second frontend cannot write the active destination.');
  assert.equal(successful(await first.call('run.pause',{id:started.runId})).accepted,true);
  successful(await first.call('run.cancel',{id:started.runId}));
  const canceled=await finished(first,started.runId);assert.equal(canceled.phase,'canceled');assert.equal(canceled.executionActive,false);
  assert.equal(successful(await first.call('run.cancel',{id:started.runId})).accepted,false);
  checks.push(mode+': cancel while paused and awaiting a response releases execution; repeated cancel is harmless.');
  model.holdReads=false;
  const recovered=successful(await other.call('retrieve.start',{...args,requestId:randomUUID()}));assert.equal((await finished(other,recovered.runId)).phase,'completed');
  checks.push(mode+': another frontend can use the destination after cancellation cleanup.');
  for(const c of [first,other]){await settled(c,'connection.disconnect');await c.close();clients.splice(clients.indexOf(c),1);}
  model.state=join(root,mode+'-crash-state');model.holdReads=true;model.holds.clear();model.allowedReads=0;
  const crash=await client(mode,model.state);const interrupted=await start(crash,mode+'-crash-output',false);await waitUntil(()=>model.holds.has(1));
  crash.child.kill();await crash.closed;clients.splice(clients.indexOf(crash),1);
  model.holdReads=false;
  const resumed=await client(mode==='cli'?'mcp':'cli',model.state);
  const saved=successful(await resumed.call('run.status',{id:interrupted.runId}));assert.equal(saved.executionActive,false);
  successful(await resumed.call('run.recover',{id:interrupted.runId}));
  const done=await finished(resumed,interrupted.runId);assert.equal(done.phase,'completed');
  assert.ok(done.artifacts.every(a=>a.acquisition==='collected'&&a.processing==='derived'));
  checks.push(mode+': the other frontend recovers an abruptly stopped owner using the saved run ID.');
  await settled(resumed,'connection.disconnect');await resumed.close();clients.splice(clients.indexOf(resumed),1);
 }
 const result={passed:true,proofClass:'Controlled loopback failures through packaged CLI and MCP',checks,nativeMutations:false,fixtureCleanup:true};
 await writeFile(evidencePath,JSON.stringify(result,null,2));console.log(JSON.stringify(result));
} finally {
 for(const c of clients)await c.close().catch(()=>{});
 server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
 assert.equal(dirname(resolve(root)), resolve(tmpdir()));
  await rm(root, { recursive: true, force: true });
}
