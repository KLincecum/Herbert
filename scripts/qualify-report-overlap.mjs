import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { randomUUID } from "node:crypto";
const [exe, plugins, evidence, clientModule] = process.argv.slice(2);
const { Client, successful } = await import(pathToFileURL(clientModule));
await mkdir(evidence, { recursive: true });
const company = "Fixture";
const rules = Array.from({ length: 5 }, (_, i) => ({
  Company: company,
  ReportID: i < 2 ? "First" : i < 4 ? "Second" : "StockParent",
  StyleNum: 1001 + i,
  SysRowID: "00000000-0000-0000-0000-00000000000" + (i + 1),
  RuleBody: "<Rule>password=Keep Ω " + i + "</Rule>",
  IsEnabled: i % 2 === 0,
}));
const parents = ["First", "Second", "StockParent"].map((ReportID, i) => ({
  Company: "",
  ReportID,
  SystemFlag: i === 2,
  SystemRpt: i === 2,
  SysRowID: "10000000-0000-0000-0000-00000000000" + (i + 1),
}));
rules.push(
  ...[6, 7].map((i) => ({
    Company: "",
    ReportID: "First",
    StyleNum: 2000 + i,
    SysRowID: "00000000-0000-0000-0000-00000000000" + i,
    RuleBody: "<Rule>global original Ω " + i + "</Rule>",
    IsEnabled: true,
  })),
);
const styles = rules.map((r) => ({
  Company: r.Company,
  ReportID: r.ReportID,
  StyleNum: r.StyleNum,
  SysRowID: r.SysRowID,
  SystemFlag: false,
  RptTypeID: "SSRS",
  PrintProgram: "reports/CustomReports/Shared",
  Description: "Keep Ω",
}));
const rdl =
  '<Report xmlns="http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition"><Code>Public Function Keep() As String\nReturn "original"\nEnd Function</Code></Report>';
let calls = [],
  errors = [],
  scenario = "success",
  client;
const checks = [];
const send = (res, status, value) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(value));
};
const server = createServer(async (req, res) => {
  try {
    let body = "";
    for await (const chunk of req) body += chunk;
    const args = JSON.parse(body || "{}");
    const parts = new URL(req.url, "http://localhost").pathname.split("/"),
      method = parts.at(-1),
      service = parts.at(-2);
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
    if (service === "Ice.BO.BAQDesignerSvc" && method === "GetFieldList") {
      calls.push({ method, table: args.dataTableID });
      const fields =
        args.dataTableID === "Report"
          ? Object.keys(parents[0])
          : args.dataTableID === "ReportStyle"
            ? Object.keys(styles[0])
            : [
                  "ReportStyleRule",
                  "RptStylePrinters",
                  "ReportStyleImage",
                ].includes(args.dataTableID)
              ? [...Object.keys(rules[0]), "SystemFlag"]
              : [
                  "Company",
                  "SysRowID",
                  "PrintProgram",
                  "Version",
                  "SystemFlag",
                ];
      return send(res, 200, {
        returnObj: {
          TableFieldList: fields.map((name) => ({
            DBSchemaName: "Ice",
            DBTableName: args.dataTableID,
            DBFieldName: name,
            DataType: ["SystemFlag", "SystemRpt", "IsEnabled"].includes(name)
              ? "bit"
              : name === "StyleNum"
                ? "int"
                : name === "SysRowID"
                  ? "uniqueidentifier"
                  : "nvarchar",
            FieldFormat: "x(200)",
          })),
        },
      });
    }
    if (service === "Ice.BO.DynamicQuerySvc" && method === "Execute") {
      const ds = args.queryDS,
        table = ds.QueryTable[0].DBTableName;
      const params = Object.fromEntries(
        args.executionParams.ExecutionParameter.map((p) => [
          p.ParameterID,
          p.ParameterValue,
        ]),
      );
      const terms = ds.QueryWhereItem;
      const term = (field) => terms.find((t) => t.FieldName === field);
      const value = (field) =>
        term(field) ? params[term(field).RValue.slice(1)] : undefined;
      calls.push({
        method,
        table,
        after: value("SysRowID"),
        company: value("Company"),
      });
      let rows;
      if (table === "ReportStyleRule") {
        assert.ok(ds.QueryRelationField.length === 3);
        assert.equal(params.customRuleStyle ?? params.customStyle, "false");
        assert.ok(ds.QueryField.some((f) => f.Alias === "RuleBody"));
        rows = rules.filter((r) => r.Company === value("Company"));
      } else if (table === "Report") {
        assert.ok(!ds.QueryField.some((f) => f.Alias === "RuleBody"));
        rows = parents;
      } else if (table === "ReportStyle") {
        rows = styles.filter((r) => r.Company === value("Company"));
      } else {
        rows = [];
      }
      if (value("SysRowID"))
        rows = rows.filter((r) => r.SysRowID > value("SysRowID"));
      rows = rows
        .slice(0, 2)
        .map((r) => ({
          ...r,
          ...("RuleBody" in r
            ? {
                HerbertRuleBodyBytes: Buffer.byteLength(
                  r.RuleBody,
                  "utf16le",
                ).toString(),
              }
            : {}),
        }));
      return send(res, 200, { returnObj: { Results: rows, Errors: [] } });
    }
    if (service === "Ice.BO.ReportSvc" && method === "ExportReportStyle") {
      calls.push({ method, ...args });
      assert.notEqual(args.reportId, "StockParent");
      const row = rules.find(
        (r) => r.ReportID === args.reportId && r.StyleNum === args.styleNumber,
      );
      assert.ok(row);
      if (scenario === "refused" && row.StyleNum === 1001)
        return send(res, 403, {
          ErrorType: "Ice.Common.SecurityAccessException",
          ErrorMessage: "Fixture refusal",
        });
      return send(res, 200, {
        returnObj: {
          Report: [parents.find((p) => p.ReportID === row.ReportID)],
          ReportStyle: [styles.find((s) => s.StyleNum === row.StyleNum)],
          ReportStyleRule: [row],
          ReportStyleImage: [],
        },
      });
    }
    if (service === "Ice.BO.ReportSvc" && method === "ExportSsrsRdl") {
      calls.push({ method, ...args });
      return send(res, 200, { returnObj: Buffer.from(rdl).toString("base64") });
    }
    if (
      service === "Ice.LIB.EfxLibraryDesignerSvc" ||
      service === "Ice.Lib.EfxLibraryDesignerSvc"
    ) {
      calls.push({ method, helperUnavailable: true });
      if (method === "GetLibrary")
        return send(res, 200, { returnObj: { EfxLibrary: [] } });
      return send(res, 404, {
        ErrorType: "Ice.Common.RecordNotFoundException",
        ErrorMessage: "Record not found",
      });
    }
    throw Error("Unexpected " + service + "." + method);
  } catch (error) {
    errors.push(String(error));
    send(res, 500, { ErrorMessage: String(error) });
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
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
        company,
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
    for (scenario of ["success", "refused"]) {
      calls = [];
      errors = [];
      const destination = join(evidence, mode + "-" + scenario);
      const run = successful(
        await client.call("backup.start", {
          requestId: randomUUID(),
          destination,
          outputMode: "separate",
          categories: ["report-styles-and-layouts", "report-behavior"],
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
        join(evidence, mode + "-" + scenario + "-status.json"),
        JSON.stringify(status, null, 2),
      );
      await writeFile(
        join(evidence, mode + "-" + scenario + "-calls.json"),
        JSON.stringify({ calls, errors }, null, 2),
      );
      assert.deepEqual(errors, []);
      assert.equal(
        status.artifacts.filter((a) =>
          ["ReportStyles", "PersistedReportStyleRule"].includes(
            a.identity.kind,
          ),
        ).length,
        14,
        JSON.stringify(status.diagnostics),
      );
      assert.equal(
        calls.filter((c) => c.method === "ExportReportStyle").length,
        6,
      );
      assert.equal(
        calls.filter((c) => c.method === "Execute" && c.table === "Report")
          .length,
        2,
        "Both plugins share one paged parent inventory",
      );
      assert.equal(
        calls.filter((c) => c.method === "ExportSsrsRdl").length,
        1,
        "Shared layout downloaded once across styles",
      );
      assert.equal(
        calls.filter(
          (c) => c.method === "Execute" && c.table === "ReportStyleRule",
        ).length,
        5,
        "Both plugins share three company rule pages plus two populated-global paging requests",
      );
      for (const artifact of status.artifacts.filter(
        (a) => a.identity.category === "report-styles-and-layouts",
      )) {
        assert.equal(
          artifact.processing,
          "derived",
          JSON.stringify(artifact.diagnostics),
        );
        assert.equal(
          artifact.acquisition,
          scenario === "refused" && artifact.identity.keys.StyleNum === "1001"
            ? "partial"
            : "collected",
          JSON.stringify(artifact.diagnostics),
        );
        const texts = await Promise.all(
          artifact.files
            .filter((f) => f.role === "derived" && f.path.endsWith(".xml"))
            .map((f) =>
              readFile(
                join(destination, status.separateFolder, f.path),
                "utf8",
              ),
            ),
        );
        assert.ok(
          texts.includes(rdl),
          "Original RDL must be retained for every selected style",
        );
      }
      for (const artifact of status.artifacts.filter(
        (a) => a.identity.category === "report-behavior",
      )) {
        const expected = rules.find(
          (r) => r.SysRowID === artifact.identity.keys.SysRowID,
        );
        assert.ok(expected);
        assert.equal(
          artifact.acquisition,
          scenario === "refused" && expected.StyleNum === 1001
            ? "partial"
            : "collected",
          JSON.stringify(artifact.diagnostics),
        );
        assert.equal(
          artifact.processing,
          "derived",
          JSON.stringify(artifact.diagnostics),
        );
        const texts = await Promise.all(
          artifact.files
            .filter((f) => f.role === "derived" && f.path.endsWith(".xml"))
            .map((f) =>
              readFile(
                join(destination, status.separateFolder, f.path),
                "utf8",
              ),
            ),
        );
        assert.ok(
          texts.includes(expected.RuleBody),
          "Original rule text must be saved",
        );
      }
      checks.push({ mode, scenario, calls, phase: status.phase });
    }
    await client.close();
    client = null;
  }
  await writeFile(
    join(evidence, "results.json"),
    JSON.stringify({ passed: true, checks }, null, 2),
  );
  console.log(
    JSON.stringify({
      passed: true,
      checks: checks.map((c) => ({
        mode: c.mode,
        scenario: c.scenario,
        phase: c.phase,
        exports: c.calls.filter((x) => x.method === "ExportReportStyle").length,
        parentPages: c.calls.filter(
          (x) => x.method === "Execute" && x.table === "Report",
        ).length,
      })),
    }),
  );
} finally {
  if (client) await client.close();
  server.close();
}
