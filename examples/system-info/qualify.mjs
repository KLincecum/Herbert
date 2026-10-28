// Exercise the actual modules through ABI 1. Each call gets a fresh instance,
// as it does in Herbert. This proves guest behavior, not host file persistence.
import assert from "node:assert/strict";
import { isUtf8 } from "node:buffer";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const paths = process.argv.slice(2);
if (paths.length !== 1 && paths.length !== 2) {
  throw new Error(
    "Supply the AssemblyScript module path and, optionally, its Rust counterpart.",
  );
}

class Example {
  constructor(module) {
    this.module = module;
    this.state = null;
    this.category = "";
  }

  async invoke(input) {
    const instance = await WebAssembly.instantiate(this.module, {});
    const guest = instance.exports;
    const bytes = Buffer.from(input, "utf8");
    const pointer = guest.hoover_alloc(bytes.length);
    new Uint8Array(guest.memory.buffer, pointer, bytes.length).set(bytes);
    const packed = guest.hoover_call(pointer, bytes.length);
    guest.hoover_free(pointer, bytes.length);
    const outputPointer = Number(packed >> 32n);
    const outputLength = Number(packed & 0xffffffffn);
    const output = Buffer.from(
      guest.memory.buffer,
      outputPointer,
      outputLength,
    );
    // A replacement character inserted by Node's decoder could hide invalid
    // bytes that the actual Rust host correctly refuses.
    assert.equal(isUtf8(output), true, "The guest must emit valid UTF-8");
    const reply = JSON.parse(output.toString("utf8"));
    guest.hoover_free(outputPointer, outputLength);
    return reply;
  }

  async call(operation, fields = {}) {
    const request = {
      abi: 1,
      operation,
      category: this.category,
      company: "",
      identity: null,
      cursor: null,
      pageSize: null,
      response: null,
      error: null,
      ...fields,
      state: { ...fields.state, _pluginState: this.state },
    };
    let reply = await this.invoke(JSON.stringify(request));
    while (reply.type === "state") {
      this.state = reply.pluginState;
      reply = reply.reply;
    }
    if (reply.type === "descriptor") this.category = reply.category.id;
    return reply;
  }
}

const modules = [];
for (const filename of paths) {
  const module = await WebAssembly.compile(await readFile(filename));
  assert.deepEqual(
    WebAssembly.Module.imports(module),
    [],
    "No ambient imports",
  );
  const names = WebAssembly.Module.exports(module).map((value) => value.name);
  for (const name of [
    "memory",
    "hoover_abi",
    "hoover_alloc",
    "hoover_call",
    "hoover_free",
  ]) {
    assert.ok(names.includes(name), `Required export: ${name}`);
  }
  modules.push(module);
}

// A stable snapshot lets the two languages be compared. Actual platform facts
// and persistence are checked separately through Herbert's shared operations.
const facts = {
  herbertVersion: "qualification-fixture",
  pluginAbi: 1,
  platform: {
    os: "windows",
    architecture: "x86_64",
    family: "windows",
    pointerWidthBits: 64,
    endianness: "little",
  },
  availableParallelism: 4,
};
const settings = {
  enabled: true,
  demoCheckbox: true,
  demoChoice: "beta",
  demoFilter:
    'Example: quotes " and a backslash \\ and Unicode Ω 😀\nSecond line',
};

for (let index = 0; index < modules.length; index += 1) {
  const language = index === 0 ? "AssemblyScript" : "Rust";

  test(`${language}: controls, state, capability and report`, async () => {
    const example = new Example(modules[index]);
    const initial = await example.call("descriptor");
    assert.equal(initial.abi, 1);
    assert.equal(initial.category.available, false);
    assert.deepEqual(
      initial.category.settings.map((item) => item.kind),
      ["checkbox", "checkbox", "choice", "filter"],
    );
    assert.equal(initial.category.settings[0].demoOnly, false);
    for (const item of initial.category.settings.slice(1)) {
      assert.equal(item.demoOnly, true);
      assert.match(item.description, /Demo \/ example — no effect/);
    }
    assert.equal(
      (await example.call("acquire")).diagnostic.code,
      "COLLECTION_DISABLED",
    );

    const configured = await example.call("configure", {
      state: { values: settings },
    });
    assert.equal(configured.type, "definition");
    assert.deepEqual(configured.definition, settings);
    assert.deepEqual(example.state.config, settings);
    assert.equal((await example.call("descriptor")).category.available, true);
    assert.equal(
      example.state.status.phase,
      "ready",
    );

    const inventory = await example.call("discover");
    assert.equal(inventory.authoritative, true);
    assert.equal(inventory.identities.length, 1);
    const action = await example.call("acquire", {
      identity: inventory.identities[0],
    });
    assert.deepEqual(action.action, { type: "systemInfo" });
    assert.equal(example.state.status.phase, "collecting");
    const complete = await example.call("acquire", {
      identity: inventory.identities[0],
      state: action.state,
      response: facts,
    });
    assert.equal(complete.type, "definition");
    assert.equal(complete.completeness, "collected");
    assert.deepEqual(complete.definition, {
      greeting: "Hello from Herbert!",
      facts,
    });
    assert.equal(example.state.status.runCount, 1);
    assert.deepEqual(example.state.status.lastReport, complete.definition);

    const moved = new Example(modules[index]);
    moved.state = JSON.parse(JSON.stringify(example.state));
    const movedDescriptor = await moved.call("descriptor");
    assert.equal(movedDescriptor.category.available, true);
    assert.deepEqual(moved.state, example.state);
    const changed = {
      ...settings,
      demoCheckbox: false,
      demoChoice: "alpha",
      demoFilter: "",
    };
    await moved.call("configure", { state: { values: changed } });
    const next = await moved.call("acquire");
    const sameReport = await moved.call("acquire", {
      state: next.state,
      response: facts,
    });
    assert.deepEqual(
      sameReport.definition,
      complete.definition,
      "Demo controls have no effect",
    );
    await moved.call("configure", {
      state: { values: { ...changed, enabled: false } },
    });
    assert.equal((await moved.call("descriptor")).category.available, false);
    assert.equal(
      (await moved.call("acquire")).diagnostic.code,
      "COLLECTION_DISABLED",
    );
  });

  test(`${language}: invalid settings preserve prior state`, async () => {
    const example = new Example(modules[index]);
    await example.call("descriptor");
    await example.call("configure", { state: { values: settings } });
    const before = structuredClone(example.state);
    for (const values of [
      { ...settings, demoChoice: "unknown" },
      { ...settings, demoCheckbox: "true" },
      { ...settings, undeclared: true },
    ]) {
      const reply = await example.call("configure", { state: { values } });
      assert.equal(reply.type, "failure");
      assert.equal(reply.diagnostic.code, "INVALID_PLUGIN_SETTINGS");
      assert.deepEqual(example.state, before);
    }
  });

  test(`${language}: trailing Unicode survives serializer boundaries`, async () => {
    const example = new Example(modules[index]);
    await example.call("descriptor");
    // Eight prefixes exercise both sides of two four-code-unit SWAR words.
    // These are regression cases, not limits on a supported filter value.
    const filters = ["Portable Ω 😀", "😀 start", '"\\\n\tΩ😀'];
    for (let length = 0; length < 8; length++) {
      filters.push("x".repeat(length) + "😀");
    }
    for (const demoFilter of filters) {
      const values = { ...settings, demoFilter };
      const reply = await example.call("configure", { state: { values } });
      assert.deepEqual(reply.definition, values);
      assert.deepEqual(example.state.config, values);
      assert.deepEqual(
        Object.fromEntries((await example.call("descriptor")).category.settings.map(item => [item.key, item.default])),
        values,
      );
    }
  });

  test(`${language}: removed operations cannot run or alter portable state`, async () => {
    const example = new Example(modules[index]);
    await example.call("descriptor");
    for (const enabled of [false, true]) {
      await example.call("configure", { state: { values: { ...settings, enabled } } });
      const before = structuredClone(example.state);
      for (const operation of ["inspect", "status"]) {
        const reply = await example.call(operation);
        assert.equal(reply.type, "failure");
        assert.equal(reply.diagnostic.code, "UNSUPPORTED_OPERATION");
        assert.deepEqual(example.state, before);
      }
    }
  });

  test(`${language}: foreign identities and invalid continuations preserve state`, async () => {
    const example = new Example(modules[index]);
    await example.call("descriptor");
    await example.call("configure", { state: { values: settings } });
    const before = structuredClone(example.state);
    const valid = (await example.call("discover")).identities[0];
    for (const identity of [
      { ...valid, category: "another-plugin" },
      { ...valid, kind: "AnotherKind" },
      { ...valid, scope: { type: "company", company: "OTHER" } },
      { ...valid, keys: { unexpected: "value" } },
    ]) {
      const reply = await example.call("acquire", { identity });
      assert.equal(reply.diagnostic.code, "INVALID_IDENTITY");
      assert.deepEqual(example.state, before);
    }
    const wrongPhase = await example.call("acquire", { identity: valid, response: facts, state: { phase: "wrong" } });
    assert.equal(wrongPhase.diagnostic.code, "INVALID_PLUGIN_STATE");
    assert.deepEqual(example.state, before);
  });

  test(`${language}: host failure records failure and permits retry`, async () => {
    const example = new Example(modules[index]);
    await example.call("descriptor");
    await example.call("configure", { state: { values: settings } });
    const error = { code: "QUALIFICATION_FAILURE", message: "Owned simulated host failure", retryable: true };
    const failed = await example.call("acquire", { error });
    assert.deepEqual(failed.diagnostic, error);
    assert.equal(example.state.status.phase, "failed");
    assert.equal(example.state.status.runCount, 0);
    const retry = await example.call("acquire");
    const complete = await example.call("acquire", { state: retry.state, response: facts });
    assert.equal(complete.completeness, "collected");
    assert.equal(example.state.status.runCount, 1);
  });

  test(`${language}: malformed invocation does not damage the next instance`, async () => {
    const example = new Example(modules[index]);
    try {
      const reply = await example.invoke("{invalid");
      assert.equal(reply.type, "failure");
    } catch (error) {
      assert.ok(error instanceof WebAssembly.RuntimeError);
    }
    assert.equal((await example.call("descriptor")).type, "descriptor");
  });
}
