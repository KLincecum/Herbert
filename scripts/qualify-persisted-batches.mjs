import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
const [exe, plugins, evidence, repo] = process.argv.slice(2);
const { Client, successful } = await import(
  pathToFileURL(join(repo, "scripts/test-client.mjs"))
);
await mkdir(evidence, { recursive: true });
const keys = [
  "Company",
  "ProductID",
  "TypeCode",
  "Key1",
  "Key2",
  "Key3",
  "CGCCode",
];
const roots = [];
const chunks = [];
for (const type of ["KNTCMetaUI", "KineticGridLayout"])
  for (const company of ["C", ""])
    for (let i = 0; i < 3; i++) {
      const id = String(roots.length + 1).padStart(12, "0");
      const row = {
        Company: company,
        ProductID: "EP",
        TypeCode: type,
        Key1: "Owner",
        Key2: "View" + i,
        Key3: type === "KNTCMetaUI" ? "" : "Grid",
        CGCCode: "",
        SystemFlag: false,
        SysRowID: "11111111-1111-1111-1111-" + id,
        Content: JSON.stringify({
          value: "Original Ω " + type + " " + company + " " + i,
        }),
        SysCharacter01: "",
        SysCharacter02: "Owner",
        SysCharacter03: "",
      };
      roots.push(row);
      for (let j = 0; j < 2; j++)
        chunks.push({
          ...Object.fromEntries(keys.map((k) => [k, row[k]])),
          SystemFlag: false,
          SeqNum: j + 1,
          SysRowID:
            "22222222-2222-2222-2222-" +
            String(chunks.length + 1).padStart(12, "0"),
          Chunk: j === 0 ? "<Source>" : "Original Ω " + id + "</Source>",
        });
    }
const descriptors = JSON.parse(
  await readFile(join(repo, "crates/herbert-plugins/src/tables.json")),
);
const fields = new Set([
  "SysRowID",
  "SystemFlag",
  "SystemCode",
  "SystemRpt",
  "SeqNum",
  "Content",
  "Chunk",
  "Description",
  "SysCharacter01",
  "SysCharacter02",
  "SysCharacter03",
  "Type",
  "SubType",
  "ViewId",
  "UserName",
  "LayerName",
  "DeviceType",
  ...keys,
]);
for (const s of descriptors) {
  for (const k of [
    ...s.keys,
    ...Object.keys(s.filters ?? {}),
    ...Object.keys(s.discovery_filters ?? {}),
    ...(s.discovery_fields ?? []),
  ])
    fields.add(k);
  for (const c of s.children ?? [])
    for (const k of Object.keys(c.links)) fields.add(k);
}
let calls = [],
  errors = [],
  client;
const send = (res, code, body) => {
  res.writeHead(code, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
function match(row, t, params) {
  const right = params[t.RValue.slice(1)];
  const left = row[t.FieldName];
  if (left === undefined) return false;
  const a = typeof left === "boolean" ? String(left) : String(left);
  switch (t.CompOp) {
    case "=":
      return a === right;
    case "<>":
      return a !== right;
    case ">":
      return a > right;
    case ">=":
      return a >= right;
    case "<=":
      return a <= right;
    default:
      throw Error("Unknown operator " + t.CompOp);
  }
}
const server = createServer(async (req, res) => {
  try {
    let body = "";
    for await (const c of req) body += c;
    const args = body ? JSON.parse(body) : {};
    const p = new URL(req.url, "http://localhost").pathname.split("/"),
      method = p.at(-1),
      service = p.at(-2);
    if (service === "Ice.Lib.SessionModSvc" && method === "GetDatabaseVersion")
      return send(res, 200, { returnObj: "5.1.100" });
    if (service === "Ice.BO.UserFileSvc")
      return send(res, 200, {
        returnObj: {
          UserFile: [
            {
              UserID: "fixture-user",
              DcdUserID: "fixture-user",
              SecurityMgr: true,
              UserDisabled: false,
              LockedOut: false,
            },
          ],
        },
      });
    calls.push({ service, method, args });
    if (method === "GetApplications") return send(res, 200, { returnObj: [] });
    if (method === "GetFieldList")
      return send(res, 200, {
        returnObj: {
          TableFieldList: [...fields].map((n) => ({
            DBSchemaName: "Ice",
            DBTableName: args.dataTableID,
            DBFieldName: n,
            DataType:
              n === "SystemFlag"
                ? "bit"
                : n === "SeqNum"
                  ? "int"
                  : n === "SysRowID"
                    ? "uniqueidentifier"
                    : "nvarchar",
            FieldFormat: "x(200)",
          })),
        },
      });
    if (service === "Ice.BO.DynamicQuerySvc" && method === "Execute") {
      const ds = args.queryDS,
        table = ds.QueryTable[0].DBTableName,
        params = Object.fromEntries(
          args.executionParams.ExecutionParameter.map((p) => [
            p.ParameterID,
            p.ParameterValue,
          ]),
        );
      let rows =
        table === "XXXDef" ? roots : table === "XXXChunk" ? chunks : [];
      rows = rows.filter((row) =>
        ds.QueryWhereItem.every((t) => {
          if (t.TableID === "BatchParent") {
            const parent = roots.find((r) =>
              keys.every((k) => r[k] === row[k]),
            );
            return parent && match(parent, t, params);
          }
          return match(row, t, params);
        }),
      );
      rows = rows
        .slice(0, 2)
        .map((r) => ({
          ...r,
          ...("Content" in r
            ? {
                HerbertContentBytes: String(
                  Buffer.byteLength(r.Content, "utf16le"),
                ),
                HerbertParentsBytes: "0",
                HerbertDraftBytes: "0",
              }
            : {}),
          ...("Chunk" in r
            ? {
                HerbertChunkBytes: String(
                  Buffer.byteLength(r.Chunk, "utf16le"),
                ),
              }
            : {}),
        }));
      return send(res, 200, { returnObj: { Results: rows, Errors: [] } });
    }
    throw Error("Unexpected " + service + "." + method);
  } catch (e) {
    errors.push(String(e));
    send(res, 500, { ErrorMessage: String(e) });
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
const checks = [];
try {
  for (const mode of ["cli", "mcp"]) {
    client = new Client(
      exe,
      mode,
      join(evidence, mode + "-state"),
      plugins,
      join(evidence, mode + "-coordination"),
    );
    await client.initialize();
    successful(
      await client.call("connection.connect", {
        url: "http://127.0.0.1:" + server.address().port,
        company: "C",
        authMode: "basicApiKey",
        credentialSource: {
          type: "inline",
          credentials: {
            username: "fixture-user",
            password: "synthetic",
            apiKey: "synthetic",
          },
        },
      }),
    );
    calls = [];
    errors = [];
    const destination = join(evidence, mode);
    const run = successful(
      await client.call("backup.start", {
        requestId: randomUUID(),
        destination,
        outputMode: "separate",
        categories: ["C12", "C13"],
        pageSize: 2,
        temporaryAccess: false,
      }),
    );
    let status;
    do {
      status = successful(await client.call("run.status", { id: run.runId }));
      if (status.executionActive) await new Promise((r) => setTimeout(r, 25));
    } while (status.executionActive);
    await writeFile(
      join(evidence, mode + "-status.json"),
      JSON.stringify(status, null, 2),
    );
    await writeFile(
      join(evidence, mode + "-calls.json"),
      JSON.stringify({ calls, errors }, null, 2),
    );
    assert.deepEqual(errors, []);
    const artifacts = status.artifacts.filter((a) =>
      ["PersistedKineticApplicationFile", "PersistedXXXDef"].includes(
        a.identity.kind,
      ),
    );
    assert.equal(artifacts.length, 12, JSON.stringify(status.diagnostics));
    for (const a of artifacts) {
      assert.equal(a.acquisition, "collected", JSON.stringify(a.diagnostics));
      assert.equal(a.processing, "derived", JSON.stringify(a.diagnostics));
      const texts = await Promise.all(
        a.files.map((f) =>
          readFile(join(destination, status.separateFolder, f.path), "utf8"),
        ),
      );
      const expected = roots.find((r) =>
        keys.every((k) => r[k] === a.identity.keys[k]),
      );
      assert.ok(expected);
      assert.ok(
        texts.some(
          (t) =>
            t.includes(expected.Content) ||
            t.includes(JSON.stringify(expected.Content).slice(1, -1)),
        ),
        "Original root retained",
      );
      assert.ok(
        texts.some((t) =>
          t.includes("Original Ω " + expected.SysRowID.slice(-12)),
        ),
        "Own chunks retained",
      );
      for (const other of roots.filter((r) => r !== expected))
        assert.ok(
          !texts.some((t) =>
            t.includes("Original Ω " + other.SysRowID.slice(-12)),
          ),
          "No foreign chunks",
        );
    }
    assert.ok(
      !calls.some((c) => c.service === "Ice.BO.KineticGridLayoutSvc"),
      "No duplicate native grid collector",
    );
    for (const type of ["KNTCMetaUI", "KineticGridLayout"])
      for (const company of ["C", ""])
        for (const table of ["XXXDef", "XXXChunk"]) {
          const selected = calls.filter((c) => {
            if (
              c.method !== "Execute" ||
              c.args.queryDS.QueryTable[0].DBTableName !== table
            )
              return false;
            const params = Object.fromEntries(
              c.args.executionParams.ExecutionParameter.map((p) => [
                p.ParameterID,
                p.ParameterValue,
              ]),
            );
            const terms = c.args.queryDS.QueryWhereItem;
            return (
              terms.some(
                (t) =>
                  t.FieldName === "TypeCode" &&
                  params[t.RValue.slice(1)] === type,
              ) &&
              terms.some(
                (t) =>
                  t.FieldName === "Company" &&
                  params[t.RValue.slice(1)] === company,
              )
            );
          });
          assert.equal(
            selected.length,
            table === "XXXDef" ? 2 : 5,
            "Only paged root/child inventory; no individual rereads",
          );
        }

    checks.push({
      mode,
      items: artifacts.length,
      phase: status.phase,
      queries: calls.filter((c) => c.method === "Execute").length,
    });
    await client.close();
    client = null;
  }
  await writeFile(
    join(evidence, "results.json"),
    JSON.stringify({ passed: true, checks }, null, 2),
  );
  console.log(JSON.stringify({ passed: true, checks }));
} finally {
  if (client) await client.close();
  server.close();
}
