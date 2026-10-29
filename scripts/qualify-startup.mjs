// Measure fresh-process discovery and exercise the actual packaged loader.
// Changes occur only in owned fixture copies. No Epicor connection is used.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
  unlink,
  writeFile,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { Client, successful } from "./test-client.mjs";

const [folder, evidence] = process.argv.slice(2);
if (!folder || !evidence)
  throw new Error("Supply the package folder and evidence JSON path.");
const packageFolder = resolve(folder);
const manifest = JSON.parse(
  await readFile(join(packageFolder, "package.json"), "utf8"),
);
const executable = join(
  packageFolder,
  manifest.files.find((f) => /^herbert(?:\.exe)?$/.test(f.path)).path,
);
const modules = manifest.files.filter((f) => f.path.endsWith(".wasm"));
assert.ok(modules.length > 0);
assert.ok(!manifest.files.some(f => /\.cwasm(?:\.json)?$/.test(f.path)));

const qualificationFolder = resolve(dirname(evidence));
await mkdir(qualificationFolder, { recursive: true });
const root = await mkdtemp(join(qualificationFolder, "startup-fixture-"));
const result = {
  proofClass:
    "Windows local package; fresh processes; OS file cache may be warm",
  packageFolder,
  fixtureRoot: root,
  nativeTarget: manifest.nativeTarget,
  modules: modules.length,
  timing: [],
  checks: [],
};
async function save() {
  await writeFile(evidence, JSON.stringify(result, null, 2) + "\n");
}

function checkCapabilities(reply, count, code) {
  const value = successful(reply);
  assert.equal(value.categories.length, count);
  assert.deepEqual(
    value.diagnostics.map((d) => d.code),
    code ? [code] : [],
  );
  return value;
}

try {
  for (const mode of ["cli", "mcp"]) {
    // Three repetitions are a measurement sample, not a product limit.
    for (let sample = 0; sample < 3; sample++) {
      const started = performance.now();
      const client = new Client(
        executable,
        mode,
        join(root, `state-${mode}-${sample}`),
        join(packageFolder, "plugins"),
      );
      try {
        await client.initialize();
        checkCapabilities(await client.call("capabilities"), modules.length);
        const firstMs = performance.now() - started;
        const warmStarted = performance.now();
        checkCapabilities(await client.call("capabilities"), modules.length);
        const warmMs = performance.now() - warmStarted;
        result.timing.push({ mode, sample: sample + 1, firstMs, warmMs });
        await save();
      } finally {
        await client.close();
      }
    }

    const plugins = join(root, `plugins-${mode}`);
    await cp(join(packageFolder, "plugins"), plugins, { recursive: true });
    const client = new Client(
      executable,
      mode,
      join(root, `state-${mode}-changes`),
      plugins,
    );
    try {
      await client.initialize();
      const initial = checkCapabilities(
        await client.call("capabilities"),
        modules.length,
      );
      assert.deepEqual(initial.categories.map(c => c.id).sort(),
        [...manifest.pluginCategories].sort());
      const module = join(
        root,
        `plugins-${mode}`,
        modules[0].path.slice("plugins/".length),
      );
      const originalWasm = await readFile(module);
      // Unsigned portable Wasm loads even with obsolete companion files present.
      const native = module.slice(0, -5) + ".cwasm";
      await writeFile(native, "obsolete native bytes");
      await writeFile(native + ".json", "obsolete signature data");
      checkCapabilities(await client.call("capabilities"), modules.length);
      await writeFile(module, "not WebAssembly");
      checkCapabilities(await client.call("capabilities"), modules.length - 1, "PLUGIN_INVALID");
      await writeFile(module, originalWasm);
      checkCapabilities(await client.call("capabilities"), modules.length);
      result.checks.push({mode, check: "Unsigned Wasm loads; obsolete signing companions ignored; invalid Wasm refused; restoration loads"});

      const example = initial.categories.find((category) =>
        category.id === "kinetic-personalizations",
      );
      assert.ok(example);
      const values = {
        itemsPerPage: 1000,
        combinedRead: true,
      };
      successful(
        await client.call("plugin.configure", { category: example.id, values }),
      );
      const exampleModule = modules.find((f) =>
        f.path.includes("Kinetic personalizations/"),
      );
      assert.ok(exampleModule);
      const oldStem = join(
        plugins,
        exampleModule.path.slice("plugins/".length),
      ).slice(0, -5);
      const newFolder = join(plugins, "moved-example");
      await mkdir(newFolder);
      const newStem = join(newFolder, "renamed");
      for (const suffix of [".wasm", ".state.json"]) {
        await rename(oldStem + suffix, newStem + suffix);
      }
      const moved = checkCapabilities(
        await client.call("capabilities"),
        modules.length,
      );
      assert.equal(
        moved.categories.find((c) => c.id === example.id).available,
        true,
      );
      const portable = JSON.parse(
        await readFile(newStem + ".state.json", "utf8"),
      );
      assert.deepEqual(portable.config, values);

      result.checks.push({
        mode,
        check:
          "Rewritten collector saves options and retains state after bundle rename",
      });

      // Companions alone declare no collection. A removed Wasm file produces no
      // expected-plugin diagnostic, even if its other bundle files remain.
      await unlink(newStem + ".wasm");
      checkCapabilities(await client.call("capabilities"), modules.length - 1);
      result.checks.push({
        mode,
        check: "Removal stays quiet; state files declare no plugin",
      });
      await save();
    } finally {
      await client.close();
    }
  }

  result.passed = true;
  await save();
} catch (error) {
  result.passed = false;
  result.failure = String(error.message ?? error);
  await save();
  throw error;
} finally {
  // The generated fixture is an immediate child of the explicitly selected
  // evidence folder. Verify that boundary before recursive cleanup on Windows.
  assert.equal(dirname(resolve(root)), qualificationFolder);
  await rm(root, { recursive: true });
}
console.log(JSON.stringify(result, null, 2));
