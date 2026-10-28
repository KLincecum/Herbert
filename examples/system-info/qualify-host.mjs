import { Client, successful } from "../../scripts/test-client.mjs";
// Test plugin options and discovery over CLI and MCP. The two modules are copied
// into owned fixture folders. No connection, credential or Epicor write is used.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  unlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

const [executable, assemblyscript, rust, evidence] = process.argv.slice(2);
if (!executable || !assemblyscript || !rust || !evidence) {
  throw new Error(
    "Supply Herbert, both module paths and an evidence JSON path.",
  );
}

async function start(mode, state, plugins) {
  const client = new Client(executable, mode, state, plugins);
  await client.initialize();
  return client;
}

const root = await mkdtemp(join(tmpdir(), "herbert-plugin-qualification-"));
const seed = {
  schemaVersion: 1,
  config: {
    enabled: false,
    demoCheckbox: false,
    demoChoice: "alpha",
    demoFilter: "",
  },
  status: { phase: "disabled", runCount: 0, lastReport: null },
};
const values = {
  enabled: true,
  demoCheckbox: true,
  demoChoice: "beta",
  demoFilter: 'Demo: Ω 😀\nquotes "',
};
const results = [];

for (const mode of ["cli", "mcp"]) {
  const plugins = join(root, mode, "plugins");
  const state = join(root, mode, "host-state");
  const fixtures = [];
  for (const [index, source] of [assemblyscript, rust].entries()) {
    const folder = join(plugins, `example-${index}`);
    await mkdir(folder, { recursive: true });
    const module = join(folder, "module.wasm");
    const config = join(folder, "module.state.json");
    await copyFile(source, module);
    await writeFile(config, JSON.stringify(seed, null, 2) + "\n");
    fixtures.push({ module, config });
  }

  let client = await start(mode, state, plugins);
  try {
    const initial = successful(await client.call("capabilities"));
    assert.equal(initial.categories.length, 2);
    assert.deepEqual(initial.diagnostics, []);
    assert.ok(
      initial.categories.every((category) => category.available === false),
    );

    for (const category of initial.categories) {
      const configured = successful(
        await client.call("plugin.configure", {
          category: category.id,
          values,
        }),
      );
      assert.deepEqual(configured.definition, values);
    }
    for (const fixture of fixtures) {
      const saved = JSON.parse(await readFile(fixture.config, "utf8"));
      assert.deepEqual(saved.config, values);
      assert.equal(saved.status.runCount, 0);
      assert.equal(saved.status.phase, "ready");
    }
    await client.close();

    // Restart with the same files, then move and rename each complete bundle.
    client = await start(mode, state, plugins);
    const restarted = successful(await client.call("capabilities"));
    assert.ok(
      restarted.categories.every((category) => category.available === true),
    );
    await client.close();

    const moved = join(root, mode, "moved-plugins");
    const renamed = [];
    for (const [index, fixture] of fixtures.entries()) {
      const folder = join(moved, `renamed-${index}`);
      await mkdir(folder, { recursive: true });
      const module = join(folder, "different-name.wasm");
      await copyFile(fixture.module, module);
      await copyFile(fixture.config, join(folder, "different-name.state.json"));
      renamed.push(module);
    }
    client = await start(mode, state, moved);
    const movedCapabilities = successful(await client.call("capabilities"));
    assert.equal(movedCapabilities.categories.length, 2);
    assert.ok(movedCapabilities.categories.every(category => category.available));
    for (const module of renamed) {
      const portable = JSON.parse(await readFile(module.replace(/\.wasm$/, ".state.json"), "utf8"));
      assert.deepEqual(portable.config, values);
      assert.equal(portable.status.runCount, 0);
    }

    // Folder removal removes the capability quietly, even with its state left.
    await unlink(renamed[0]);
    const removed = successful(await client.call("capabilities"));
    assert.equal(removed.categories.length, 1);
    assert.deepEqual(removed.diagnostics, []);
    await writeFile(join(moved, "invalid-present.wasm"), "invalid module");
    const invalid = successful(await client.call("capabilities"));
    assert.equal(invalid.categories.length, 1);
    assert.ok(
      invalid.diagnostics.some(
        (diagnostic) => diagnostic.code === "PLUGIN_INVALID",
      ),
    );
    results.push({
      mode,
      checks: [
        "disabled startup",
        "all control values saved",
        "independent state readback",
        "restart",
        "bundle move and rename",
        "quiet folder removal",
        "present-file failure containment",
      ],
    });
  } finally {
    if (!client.child.stdin.writableEnded) await client.close();
  }
}

const hashes = {};
for (const [name, filename] of Object.entries({
  executable,
  assemblyscript,
  rust,
})) {
  hashes[name] = createHash("sha256")
    .update(await readFile(filename))
    .digest("hex");
}
await mkdir(dirname(evidence), { recursive: true });
await writeFile(
  evidence,
  JSON.stringify({ fixtureRoot: root, hashes, results }, null, 2) + "\n",
);
console.log(`CLI and MCP plugin qualification passed. Evidence: ${evidence}`);
