// Exercise the packaged helper lifecycle against a synthetic local server.
// No lab connection or real credentials are used.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import { randomUUID, createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
const [workspace, packageDir, evidenceDir] = process.argv.slice(2);
const { Client, successful } = await import(
  pathToFileURL(join(workspace, "scripts/test-client.mjs"))
);
await mkdir(evidenceDir, { recursive: true });
const company = "FIXTURE",
  caller = "fixture-caller";
const zip = Buffer.from(
  "UEsDBBQAAAAIAKutU13qoU6cbwAAAIUAAAALAAAARml4dHVyZS5yZGwtjcENwjAMAFeJOkDd8uCBQj4MgMQGVeq0lhK72AZ1fFDhe6fTxQduoh72Vtmu3eq+XQAsr9gm6xtlFZPifZYG9qyG+kYFPSLiBU7DeIZh/JMZCzE5CXcp3mTGdFdaiKcaCu3+Ugz5SyMcLsJvnj5QSwECFAAUAAAACACrrVNd6qFOnG8AAACFAAAACwAAAAAAAAAAAAAAgAEAAAAARml4dHVyZS5yZGxQSwUGAAAAAAEAAQA5AAAAmAAAAAAA",
  "base64",
);
const sha = createHash("sha256").update(zip).digest("hex");
const model = { libraries: new Map(), requests: [], errors: [] };
const send = (r, status, obj) => {
  r.writeHead(status, { "content-type": "application/json" });
  r.end(JSON.stringify(obj));
};
const server = createServer(async (req, res) => {
  try {
    let body = "";
    for await (const chunk of req) body += chunk;
    const args = JSON.parse(body || "{}");
    const url = new URL(req.url, "http://127.0.0.1");
    const method = url.pathname.split("/").at(-1);
    model.requests.push({ method, args });
    if (method === "IsValidLibraryId") return send(res, 200, { returnObj: "" });
    if (method === "GetDefaults")
      return send(res, 200, {
        returnObj: { EfxLibrary: [{ EpicorVersion: "5.1.100" }] },
      });
    if (method === "GetDatabaseVersion")
      return send(res, 200, { returnObj: "5.1.100" });
    if (method === "GetUserFile")
      return send(res, 200, {
        returnObj: {
          UserFile: [
            {
              DcdUserID: caller,
              UserID: caller,
              SecurityMgr: true,
              UserDisabled: false,
              LockedOut: false,
            },
          ],
        },
      });
    if (method === "GetLibrary") {
      const item = model.libraries.get(args.libraryId);
      if (!item)
        return send(res, 200, {
          returnObj: { EfxLibrary: [], EfxFunction: [] },
        });
      const p = item.package,
        l = p.Library;
      return send(res, 200, {
        returnObj: {
          EfxLibrary: [
            {
              LibraryID: p.LibraryId,
              OwnedByCompany: l.OwnedByCompany,
              Owner: l.Owner,
              GlobalID: l.GlobalID,
              SysRowID: item.rowId,
              Notes: l.Notes,
              Disabled: l.Disabled,
            },
          ],
          EfxFunction: l.Functions.map((f) => ({
            LibraryID: p.LibraryId,
            FunctionID: f.FunctionID,
            Disabled: f.Disabled,
            Invalid: f.Invalid,
          })),
        },
      });
    }
    if (method === "ImportLibrary") {
      const bytes = Buffer.from(args.libraryPackage, "base64");
      const p = JSON.parse(bytes.toString("utf8").replace(/^\uFEFF/, ""));
      assert.equal(args.options.OverwriteMode, 0);
      assert.equal(args.options.NewLibraryId, p.LibraryId);
      assert.equal(model.libraries.has(p.LibraryId), false);
      model.libraries.set(p.LibraryId, { package: p, rowId: randomUUID() });
      return send(res, 200, { returnObj: { BOUpdError: [] } });
    }
    if (method === "ExportLibrary") {
      const p = model.libraries.get(args.libraryID)?.package;
      assert.ok(p);
      return send(res, 200, {
        returnObj: Buffer.from(JSON.stringify(p)).toString("base64"),
      });
    }
    if (method === "ApplyChangesWithDiagnostics") {
      const rows = args.libraryTableset.EfxLibrary;
      assert.equal(rows.length, 1);
      const row = rows[0],
        item = model.libraries.get(row.LibraryID);
      assert.ok(item);
      assert.equal(row.SysRowID, item.rowId);
      if (row.RowMod === "D") {
        model.libraries.delete(row.LibraryID);
      } else {
        item.package.Library.Disabled = row.Disabled;
        for (const f of args.libraryTableset.EfxFunction) {
          const target = item.package.Library.Functions.find(
            (x) => x.FunctionID === f.FunctionID,
          );
          assert.ok(target);
          target.Disabled = f.Disabled;
          target.Invalid = false;
        }
      }
      return send(res, 200, { returnObj: {}, diagnostics: [] });
    }
    if (method === "PromoteToProduction") {
      const item = model.libraries.get(args.libraryID);
      assert.ok(item);
      item.package.Library.Published = true;
      return send(res, 200, { returnObj: {} });
    }
    if (method === "GetFieldList")
      return send(res, 200, { returnObj: { TableFieldList: [] } });
    if (method === "Execute")
      return send(res, 200, { returnObj: { Results: [], Errors: [] } });
    if (method === "Inventory") {
      const rows = ["A", "B", "C"]
        .filter((k) => !args.iHasCursor || k > args.iAfter)
        .slice(0, args.iPageSize);
      return send(res, 200, {
        oSuccess: true,
        oDS: {
          Store: rows.map((PrintProgram) => ({ PrintProgram })),
          Cursor: [{ More: rows.at(-1) !== "C", After: rows.at(-1) }],
        },
      });
    }
    if (method === "Read") {
      return send(res, 200, {
        oSuccess: true,
        oContent: zip.toString("base64"),
        oDS: {
          Metadata: [
            {
              PrintProgram: args.iPrintProgram,
              Length: zip.length,
              SHA256: sha,
              ContentHash: sha,
              SysRowID: randomUUID(),
              SysRevID: "1",
              CreatedBy: "fixture",
              CreatedOn: "2026-10-19",
              ModifiedBy: "fixture",
              ModifiedOn: "2026-10-19",
            },
          ],
        },
      });
    }
    throw Error("Unexpected route " + method);
  } catch (e) {
    model.errors.push(e.message);
    send(res, 500, {
      ErrorType: "FixtureContractError",
      ErrorMessage: e.message,
    });
  }
});
server.listen(0, "127.0.0.1");
await once(server, "listening");
let client;
const checks = [];
try {
  for (const mode of ["cli", "mcp"]) {
    client = new Client(
      join(packageDir, "herbert.exe"),
      mode,
      join(evidenceDir, mode + "-state"),
      join(packageDir, "plugins"),
      join(evidenceDir, mode + "-coordination"),
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
            username: caller,
            password: "synthetic",
            apiKey: "synthetic",
          },
        },
      }),
    );
    model.requests = [];
    model.errors = [];
    const run = successful(
      await client.call("backup.start", {
        requestId: randomUUID(),
        destination: join(evidenceDir, mode),
        outputMode: "separate",
        categories: ["C17"],
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
      join(evidenceDir, mode + "-status.json"),
      JSON.stringify(status, null, 2),
    );
    await writeFile(
      join(evidenceDir, mode + "-requests.json"),
      JSON.stringify(model, null, 2),
    );
    assert.deepEqual(model.errors, []);
    const artifacts = status.artifacts.filter(
      (a) => a.identity.kind === "CurrentCustomReportPackage",
    );
    assert.equal(artifacts.length, 3, JSON.stringify(status.diagnostics));
    for (const a of artifacts)
      assert.equal(a.acquisition, "collected", JSON.stringify(a.diagnostics));
    assert.equal(model.libraries.size, 0, "Task-owned helper deleted");
    assert.equal(
      model.requests.filter((r) => r.method === "ImportLibrary").length,
      1,
    );
    assert.equal(
      model.requests.filter((r) => r.method === "ExportLibrary").length,
      5,
      "Four setup/source checks and one fresh cleanup check; none per inventory/read call",
    );
    assert.equal(model.requests.filter((r) => r.method === "Read").length, 3);
    assert.equal(
      model.requests.filter((r) => r.method === "Inventory").length,
      2,
    );
    checks.push({
      mode,
      exports: model.requests.filter((r) => r.method === "ExportLibrary")
        .length,
      reads: 3,
      inventoryPages: 2,
      phase: status.phase,
    });
    await client.close();
    client = null;
  }
  await writeFile(
    join(evidenceDir, "results.json"),
    JSON.stringify({ passed: true, checks }, null, 2),
  );
  console.log(JSON.stringify({ passed: true, checks }));
} finally {
  if (client) await client.close();
  server.close();
}
