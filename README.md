# Herbert

<img src="assets/branding/herbert-vacuum.png" alt="Herbert vacuum" width="180">

**Your customizations. Your backup.**

Herbert downloads Epicor customization definitions and native artifacts into a local backup.
Put in your connection information, choose your plugins, and download.
Herbert keeps native exports and readable custom code together in folders you can understand.
It reads inventories in pages, downloads each required source once, and extracts readable files locally. Shared sources are reused within the run.
The standard package includes 24 Epicor collection plugins, compiled BPM/Function/UBAQ support, and the managed decompiler companion. The plugins exclude artifacts marked as System. Personalizations, compiled DLL downloads, and decompilation are off by default.
Use the desktop application, a JSON command interface, or an MCP client.

## Start here

Extract the complete package and start Herbert.
Enter the Epicor instance address, company, and credentials on Connection.
Choose collections on Plugins, select an output folder on Download, and start the download.
The account must be an enabled Security Manager. Native operation permissions still apply.

Read the [complete guide](docs/Herbert-Guide.md) for setup, collection coverage, settings, recovery, build instructions, and plugin development.
See the guide’s [known limits](docs/Herbert-Guide.md#known-limits) for cases that are not yet fully tested.
Read the [MCP guide](docs/MCP-GUIDE.md) for setup, schemas, workflows, effects, recovery, and every tool.
The application also contains user and technical help.

## What you get

- Readable definitions and source where the native interface exposes them.
- Native packages, report layouts, and resources where available.
- Durable records for transfer recovery and cleanup.
- Portable Wasm plugins with configuration stored beside each module.

Review item failures and cleanup warnings after every download.
The final screen separates downloaded, partial, failed, absent, and excluded items, with results for each plugin.
Items processed includes unsuccessful items. A plugin with no custom items can finish without creating a folder.
Successful export does not prove restore, report rendering, or business behavior.

## Build

Use Rust 1.99 or later and Python 3.12 or later.
Windows requires MSVC C++ build tools and the Windows SDK resource compiler.

```text
rustup target add wasm32-unknown-unknown
python scripts/build_workspace.py
python scripts/build.py
```

Use `--output <new-directory>` to write the package to a folder you choose.

## License

Herbert uses the [Herbert Source-Available License 1.0](LICENSE.md).
Personal and internal business use, modifications, and free sharing are allowed.
You may charge for installation, support, and customization.
Selling Herbert or providing it as a paid hosted service requires a separate written agreement.
Preserve the supplied third-party licenses and notices.

## Contributions

Kevin Lincecum maintains the official project. The project does not accept pull requests or outside code contributions.
