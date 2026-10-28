# Herbert system-info examples

These two small plugins perform the same task. One uses Rust. One uses
AssemblyScript. Each plugin requests a curated system-info snapshot from Herbert.
The snapshot describes the computer that runs Herbert. It does not describe the
Epicor server.

Herbert includes the AssemblyScript bundle and disables it by default. The Rust
source is also available. The two collections have
different IDs, so both can be installed at the same time.

## Read the code

- `rust/src/lib.rs` contains the Rust example. `rust/src/abi.rs` contains its byte bridge.
- `assemblyscript/assembly/plugin.ts` contains the AssemblyScript example.
  `config.ts` defines its state. `replies.ts` creates replies and controls.
  `abi.ts` contains its byte bridge.
- The four `hoover_` export names are ABI 1 names. Their historical spelling is
  retained for compatibility. The product is Herbert.

Neither module has WebAssembly imports. Each module requests `systemInfo` through
the host contract. It does not receive a network client, filesystem path,
environment variables, passwords or API keys.

## Controls and portable state

`enabled` is a working checkbox. It permits this plugin to gather system info.
The other three controls demonstrate a checkbox, a choice and a text filter.
Herbert saves their values. They do not change the system-info report. Their
descriptions and declarations mark them as **Demo / example — no effect**.

The module owns its JSON state. Its config, status, run count and last report are
stored together. Herbert writes that opaque value beside the actual module.
For `system-info.wasm`, the state file is `system-info.state.json`. Copy both
files to move the config and status. If you rename the module, rename the state
file to the same basename. The local `.state.lock` file is coordination data.
It is not needed when you move a closed bundle.

Save the declared options on Plugins or through the shared plugin.configure operation.
When enabled and selected, the plugin gathers its snapshot during a normal download.
Its opaque state stays beside the module. There is no separate plugin inspector
or standalone plugin-run operation.

Removing a module removes its collection from discovery. Herbert does not keep
an expected-plugin registry. A state file left behind does not create a gap.

The portable bundle consists of the Wasm module and its state file. Plugins need no signing
key, signature file, native companion or matching host build.
Herbert validates and compiles the module and checks its ABI and declared
capabilities before use.

## Build

For Rust, install the `wasm32-unknown-unknown` Rust target. In `rust`, run:

```text
cargo build --locked --release --target wasm32-unknown-unknown
```

For AssemblyScript, use Node 24 LTS, the tested runtime. In `assemblyscript`, run:

```text
npm ci --ignore-scripts
node build.mjs
```

The lockfiles record the dependency versions. The build script invokes the
compiler directly. It does not launch a shell. The AssemblyScript module uses
the minimal runtime. Herbert creates a fresh instance for each call and then
discards its managed objects. The byte bridge frees only its raw heap buffers.

The build selects json-as's `NAIVE` mode. The tested 1.6.1 `SWAR` writer corrupts
some strings that end with a UTF-16 surrogate pair, such as `Portable Ω 😀`.
The scalar writer preserves these values. `qualify.mjs` checks the output bytes
for valid UTF-8 before decoding them and checks emoji at word boundaries.
Node's usual UTF-8 decoder can insert replacement characters and hide this
failure. The example uses the supported scalar mode. The build does not patch library source.

The parent `build.py` builds both bundles. Supply `--cargo`, `--node` and
`--output`. An optional `--assemblyscript-source` selects a prepared build copy
with the pinned dependencies installed. The builder preserves existing state
in the output directory.

The separate product builder, `scripts/build.py`, packages portable Wasm modules.
Herbert compiles them when loading and reuses compiled modules while their
source bytes remain unchanged. Configuration stays beside each module, so a
plugin does not need to be rebuilt with the host to retain its settings.

## Verification

`qualify.mjs` checks the standalone ABI. `qualify-host.mjs` checks host integration through CLI and MCP.
The checks cover disabled startup, controls, Unicode, saved state, restart, moved bundles, renamed bundles, removal, and invalid modules.
Run these checks for the module and host versions you intend to distribute.
These checks do not establish Epicor collection coverage or native GUI behavior.
