// Test the actual executable against an isolated HTTP fixture.
// Arguments: executable, plugin directory, evidence directory, repository.
// Each target must save five definitions from two pages, without another read.
// Unrelated schema reads fail so later collectors must prove they still run.
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
const families = JSON.parse(
  await readFile(join(repo, "crates/herbert-plugins/src/families.json")),
);
const contracts = JSON.parse(
  await readFile(
    join(repo, "crates/herbert-plugins/src/native-child-contracts.json"),
  ),
);
const kinds = [
  "PredictiveSearch",
  "Theme",
  "Language",
  "ServiceDefinition",
  "DatasetDefinition",
  "UDMapping",
  "BPMDataForm",
  "SystemAgent",
  "GlobalAlert",
  "ColumnSecurity",
  "ConfiguratorLookup",
];
const targets = families.filter((f) => kinds.includes(f.kind));
await mkdir(evidence, { recursive: true });
let calls = [],
  errors = [],
  client;
function data(spec, page) {
  const roots = Array.from({ length: 5 }, (_, i) => ({
    ...Object.fromEntries(
      spec.keys.map((k) => [
        k,
        k === "Company"
          ? "C"
          : ["Alertnum", "MapTableID"].includes(k)
            ? i + 1
            : k === "TypeCode"
              ? "KineticGridLayout"
              : k === "ProductID"
                ? "EP"
                : spec.kind + "-" + i,
      ]),
    ),
    SystemFlag: false,
    SysRowID: "11111111-1111-1111-1111-" + String(i + 1).padStart(12, "0"),
    Description: "Keep original Ω " + i,
    Content: '{"value":"Keep original Ω"}',
  }));
  const selected = roots.slice((page - 1) * 3, page * 3);
  const out = { [spec.root]: selected };
  for (const table of spec.children) {
    const contract = contracts.find(
      (c) =>
        c.service === spec.service && c.root === spec.root && c.table === table,
    );
    out[table] = selected.map((root, i) => ({
      ...Object.fromEntries(
        Object.entries(contract.keys).map(([k, t]) => [
          k,
          contract.links[k]
            ? root[contract.links[k]]
            : ["Edm.Int32", "Edm.Int64"].includes(t)
              ? i
              : t === "Edm.Guid"
                ? root.SysRowID
                : (root[contract.links[k]] ?? table + "-" + i),
        ]),
      ),
      SystemFlag: false,
      Description: "Child for " + root[spec.keys.at(-1)],
    }));
  }
  return out;
}
const send = (res, code, body) => {
  res.writeHead(code, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
};
const server = createServer(async (req, res) => {
  try {
    let body = "";
    for await (const c of req) body += c;
    const args = body ? JSON.parse(body) : {};
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
    calls.push({ service, method, args });
    const spec = targets.find((f) => f.service === service);
    if (spec) {
      assert.equal(method, "GetRows", "No list or individual reread");
      assert.equal(args.pageSize, 3);
      assert.ok([1, 2].includes(args.absolutePage), "Only two inventory pages");
      return send(res, 200, {
        returnObj: data(spec, args.absolutePage),
        parameters: { morePages: args.absolutePage === 1 },
      });
    }
    if (service === "Ice.BO.BpMethodSvc" && method === "GetAvailableReferences")
      return send(res, 200, { returnObj: [] });
    const other = families.find((f) => f.service === service);
    if (other && method === other.list.method)
      return send(res, 200, {
        returnObj: { [other.list_table]: [] },
        parameters: { morePages: false },
      });
    if (method === "GetFieldList")
      return send(res, 200, { returnObj: { TableFieldList: [] } });
    if (service === "Ice.BO.DynamicQuerySvc" && method === "Execute")
      return send(res, 200, { returnObj: { Results: [], Errors: [] } });
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
        categories: [...new Set(targets.map((f) => f.category))],
        pageSize: 3,
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
    for (const spec of targets) {
      const artifacts = status.artifacts.filter(
        (a) => a.identity.kind === spec.kind,
      );
      assert.equal(artifacts.length, 5, spec.kind);
      assert.equal(
        calls.filter((c) => c.service === spec.service).length,
        2,
        spec.kind + " must use two pages and zero rereads",
      );
      for (const artifact of artifacts) {
        assert.equal(
          artifact.acquisition,
          "collected",
          JSON.stringify(artifact.diagnostics),
        );
        assert.equal(
          artifact.processing,
          "derived",
          JSON.stringify(artifact.diagnostics),
        );
        const saved = await Promise.all(
          artifact.files.map((f) =>
            readFile(join(destination, status.separateFolder, f.path), "utf8"),
          ),
        );
        assert.ok(
          saved.some((text) => text.includes("Keep original Ω")),
          "Original values retained",
        );
      }
      checks.push({ mode, kind: spec.kind, items: 5, requests: 2 });
    }
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
