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
  category: "ud-codes",
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
  for (const mode of ["cli", "mcp"]) {
    model.state = join(root, mode + "-download-state");
    model.coordination = join(root, mode + "-download-coordination");
    await mkdir(model.state);
    const first = await client(mode, model.state);
    const beforeAssignments = model.assignments;
    const beforeRemovals = model.removals;
    const refused = await finished(first, (await start(first, mode + "-refused-download", false)).runId);
    assert.ok(refused.artifacts.some(a => a.diagnostics.some(d => d.code === "ACCESS_DENIED")));
    assert.equal(model.assignments, beforeAssignments);
    checks.push(mode + ": unchecked authority does not grant a group.");

    // Two allowed native IDs and two denied IDs. The first allowed group is
    // also denied; only READERS_TEST is a valid grant. Account groups use '~'.
    const allowed = await finished(first, (await start(first, mode + "-allowed-download", true)).runId);
    assert.equal(allowed.phase, "completed");
    assert.equal(model.intentChecked, true);
    assert.equal(model.assignments, beforeAssignments + 1);
    assert.equal(model.removals, beforeRemovals + 1);
    assert.deepEqual(model.groups, ["BASE"]);
    assert.ok(allowed.accessLeases.length > 0);
    assert.ok(allowed.accessLeases.every(l => l.group === "READERS_TEST" && l.state === "released"));
    checks.push(mode + ": comma-separated allow and deny IDs skip the excluded candidate, grant the allowed group and restore tilde-separated account membership.");

    // The caller belongs to the second denied ID. No allowed grant can remedy
    // this refusal, and no write intent or native assignment may occur.
    model.noEntryList = "BLOCKED_TEST,BASE";
    const denyAssignments = model.assignments;
    const denyRemovals = model.removals;
    const denied = await finished(first, (await start(first, mode + "-denied-download", true)).runId);
    assert.ok(denied.artifacts.some(a => a.diagnostics.some(d => d.code === "ACCESS_DENIED")));
    assert.equal(model.assignments, denyAssignments);
    assert.equal(model.removals, denyRemovals);
    assert.deepEqual(denied.accessLeases, []);
    assert.deepEqual(model.groups, ["BASE"]);
    checks.push(mode + ": membership in a later comma-separated denied ID refuses before any grant.");
    model.noEntryList = "BLOCKED_TEST,OTHER_DENIED";
    await settled(first, "connection.disconnect");
    await first.close();
    clients.splice(clients.indexOf(first), 1);
  }

  model.state = join(root, "overlap-state");
  model.coordination = join(root, "overlap-coordination");
  await mkdir(model.state);
  model.allowedReads = 0;
  model.holdReads = true;
  const a = await client("cli", model.state);
  const runA = await start(a, "run-a", true);
  await waitUntil(() => model.holds.has(1));
  const b = await client("mcp", join(root, "different-state"));
  const runB = await start(b, "run-b", false);
  await waitUntil(() => model.holds.has(2));
  model.groups.push("LATER");
  model.revision++;
  model.holds.get(1)();
  assert.equal((await finished(a, runA.runId)).phase, "completed");
  assert.ok(model.groups.includes("READERS_TEST"));
  checks.push(
    "The first run preserves a grant while another frontend with a different state directory borrows it with its checkbox off.",
  );
  model.holds.get(2)();
  assert.equal((await finished(b, runB.runId)).phase, "completed");
  assert.deepEqual(model.groups, ["BASE", "LATER"]);
  checks.push(
    "The last holder restores only its group and preserves a later unrelated membership.",
  );
  model.holdReads = false;
  for (const c of [a, b]) {
    await settled(c, "connection.disconnect");
    await c.close();
    clients.splice(clients.indexOf(c), 1);
  }

  model.state = join(root, "uncertain-state");
  model.coordination = join(root, "uncertain-coordination");
  await mkdir(model.state);
  model.mode = "lostGrantResponse";
  const uncertain = await client("mcp", model.state);
  const beforeAssignments = model.assignments;
  const beforeRemovals = model.removals;
  const lost = await finished(uncertain, (await start(uncertain, "uncertain-download", true)).runId);
  const lostDiagnostics = [...lost.diagnostics, ...lost.artifacts.flatMap(a => a.diagnostics)];
  assert.ok(lostDiagnostics.some(d => d.code === "ACCESS_GRANT_UNCERTAIN"));
  const saved = await registry();
  const lease = Object.values(saved.leases)[0];
  const reconciled = await settled(uncertain, "helpers.reconcile", { id: lease.holder });
  assert.equal(reconciled.cleanupConfirmed, false);
  assert.equal(model.assignments, beforeAssignments + 1);
  assert.equal(model.removals, beforeRemovals);
  checks.push(
    "A lost grant response leaves an explicit uncertain intent and is not replayed or blindly removed.",
  );
  await settled(uncertain, "connection.disconnect");
  await uncertain.close();
  clients.splice(clients.indexOf(uncertain), 1);
  assert.equal(JSON.stringify(saved).includes(secret), false);
  checks.push("Access ownership evidence contains no supplied credential.");
  const result = {
    proofClass: "Isolated HTTP fixture through actual CLI and MCP",
    checks,
    nativeMutations: false,
    success: true,
  };
  await writeFile(evidencePath, JSON.stringify(result, null, 2) + "\n");
  process.stdout.write(JSON.stringify(result) + "\n");
} finally {
  for (const c of clients) await c.close().catch(() => {});
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
  assert.equal(dirname(resolve(root)), resolve(tmpdir()));
  await rm(root, { recursive: true, force: true });
}
