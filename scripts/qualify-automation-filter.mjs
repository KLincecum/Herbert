import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile, writeFile, mkdir, cp, readdir } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
const [exe, plugins, evidence, repo] = process.argv.slice(2);
const { Client, successful } = await import(
  pathToFileURL(join(repo, "scripts/test-client.mjs"))
);
await mkdir(evidence, { recursive: true });
const roots = Array.from({ length: 3 }, (_, i) => ({
  Company: "C",
  SystemCode: "ERP",
  ProcessID: "Process" + i,
  SysRowID: "11111111-1111-1111-1111-" + String(i + 1).padStart(12, "0"),
  SystemFlag: false,
  Password: "FIELD-SECRET-" + i,
}));
const tasks = roots.map((r) => ({ ...r, SysTaskNum: 7 }));
const params = roots.map((r, i) => ({
  ...r,
  SysTaskNum: 7,
  ParamName: "ClientSecret",
  ParamCharacter: "PARAM-SECRET-" + i,
  ParamToken: "TOKEN-SECRET-" + i,
  SysRevID: "1",
}));
const fields = [
  ...new Set([
    ...Object.keys(params[0]),
    "ParamDataType",
    "ParamDate",
    "ParamDecimal",
    "ParamInteger",
    "ParamLong",
    "ParamLabel",
    "ParamLogical",
    "ParamType",
    "MessageTemplateID",
    "LangNameID",
  ]),
];
let calls = [],
  errors = [],
  filtered = false,
  client;
const send = (r, c, b) => {
  r.writeHead(c, { "content-type": "application/json" });
  r.end(JSON.stringify(b));
};
const server = createServer(async (req, res) => {
  try {
    let b = "";
    for await (const d of req) b += d;
    const a = b ? JSON.parse(b) : {},
      p = new URL(req.url, "http://localhost").pathname.split("/"),
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
    calls.push({ service, method, args: a });
    if (service === "Ice.BO.ProcessSetSvc") {
      assert.equal(method, "GetRows");
      const selected = roots.slice(
        (a.absolutePage - 1) * 2,
        a.absolutePage * 2,
      );
      return send(res, 200, {
        returnObj: {
          ProcessSet: selected,
          ProcessTask: tasks.filter((t) =>
            selected.some((r) => r.ProcessID === t.ProcessID),
          ),
        },
        parameters: { morePages: a.absolutePage === 1 },
      });
    }
    if (service === "Ice.BO.SysAgentSvc")
      return send(res, 200, {
        returnObj: { SysAgent: [] },
        parameters: { morePages: false },
      });
    if (service === "Erp.BO.GlbAlertSvc")
      return send(res, 200, {
        returnObj: { GlbAlert: [] },
        parameters: { morePages: false },
      });
    if (method === "GetFieldList")
      return send(res, 200, {
        returnObj: {
          TableFieldList: fields.map((n) => ({
            DBSchemaName: "Ice",
            DBTableName: a.dataTableID,
            DBFieldName: n,
            DataType:
              n === "SystemFlag"
                ? "bit"
                : n === "SysTaskNum"
                  ? "int"
                  : n === "SysRowID"
                    ? "uniqueidentifier"
                    : "nvarchar",
            FieldFormat: "x(200)",
          })),
        },
      });
    if (service === "Ice.BO.DynamicQuerySvc" && method === "Execute") {
      const ds = a.queryDS,
        table = ds.QueryTable[0].DBTableName;
      if (table !== "ProcessTaskParam")
        return send(res, 200, { returnObj: { Results: [], Errors: [] } });
      const ps = Object.fromEntries(
        a.executionParams.ExecutionParameter.map((p) => [
          p.ParameterID,
          p.ParameterValue,
        ]),
      );
      assert.equal(ds.QueryRelationField.length, 7);
      assert.ok(
        !ds.QueryWhereItem.some((t) => t.FieldName === "ProcessID"),
        "No per-process query",
      );
      assert.equal(
        ds.QueryWhereItem.filter((t) => t.FieldName === "SystemFlag").length,
        3,
      );
      const field = ds.QueryField.find((f) => f.Alias === "ParamCharacter");
      assert.equal(
        field.TableID === "Calculated",
        filtered,
        "Setting must control server projection",
      );
      let rows = params
        .filter(
          (r) =>
            r.Company === ps.Company && (!ps.after || r.SysRowID > ps.after),
        )
        .slice(0, 2);
      if (filtered)
        rows = rows.map((r) => ({
          ...r,
          ParamCharacter: null,
          ParamToken: null,
        }));
      rows = rows.map((r) =>
        Object.fromEntries(
          Object.entries(r).filter(([k]) =>
            ds.QueryField.some((f) => f.Alias === k),
          ),
        ),
      );
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
async function allTexts(dir) {
  let out = [];
  for (const item of await readdir(dir, { withFileTypes: true })) {
    const p = join(dir, item.name);
    out.push(
      ...(item.isDirectory() ? await allTexts(p) : [await readFile(p, "utf8")]),
    );
  }
  return out;
}
try {
  for (const mode of ["cli", "mcp"]) {
    const localPlugins = join(evidence, mode + "-plugins");
    await mkdir(localPlugins, { recursive: true });
    await cp(
      join(plugins, "Automation and integrations"),
      join(localPlugins, "Automation and integrations"),
      { recursive: true },
    );
    for (filtered of [false, true]) {
      client = new Client(
        exe,
        mode,
        join(evidence, mode + "-state"),
        localPlugins,
        join(evidence, mode + "-coordination"),
      );
      await client.initialize();
      if (filtered) {
        successful(
          await client.call("plugin.configure", {
            category: "automation-and-integrations",
            values: { filterSensitiveFields: true },
          }),
        );
        await client.close();
        client = new Client(
          exe,
          mode,
          join(evidence, mode + "-state"),
          localPlugins,
          join(evidence, mode + "-coordination"),
        );
        await client.initialize();
      }
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
      const destination = join(evidence, mode + "-" + filtered);
      const run = successful(
        await client.call("backup.start", {
          requestId: randomUUID(),
          destination,
          outputMode: "separate",
          categories: ["automation-and-integrations"],
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
        join(evidence, mode + "-" + filtered + "-status.json"),
        JSON.stringify(status, null, 2),
      );
      await writeFile(
        join(evidence, mode + "-" + filtered + "-calls.json"),
        JSON.stringify({ calls, errors }, null, 2),
      );
      assert.deepEqual(errors, []);
      assert.equal(
        status.artifacts.length,
        3,
        JSON.stringify(status.diagnostics),
      );
      assert.equal(
        calls.filter((c) => c.service === "Ice.BO.ProcessSetSvc").length,
        2,
      );
      assert.equal(
        calls.filter(
          (c) =>
            c.method === "Execute" &&
            c.args.queryDS.QueryTable[0].DBTableName === "ProcessTaskParam",
        ).length,
        2,
        "One shared two-page parameter inventory",
      );
      for (const a of status.artifacts) {
        assert.equal(a.acquisition, "collected", JSON.stringify(a.diagnostics));
        assert.equal(a.processing, "derived", JSON.stringify(a.diagnostics));
        const texts = await Promise.all(
          a.files.map((f) =>
            readFile(join(destination, status.separateFolder, f.path), "utf8"),
          ),
        );
        const i = Number(a.identity.keys.ProcessID.slice(-1));
        for (const marker of [
          "FIELD-SECRET-",
          "PARAM-SECRET-",
          "TOKEN-SECRET-",
        ])
          assert.equal(
            texts.some((t) => t.includes(marker + i)),
            !filtered,
            "Saved values follow plugin setting",
          );
      }
      if (filtered) {
        const texts = await allTexts(join(destination, status.separateFolder));
        for (const marker of [
          "FIELD-SECRET-",
          "PARAM-SECRET-",
          "TOKEN-SECRET-",
        ])
          assert.ok(
            !texts.some((t) => t.includes(marker)),
            "Filtered values must not survive in raw files",
          );
      }
      checks.push({
        mode,
        filtered,
        phase: status.phase,
        items: status.artifacts.length,
        parameterPages: 2,
      });
      await client.close();
      client = null;
    }
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
