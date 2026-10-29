# Herbert guide

Herbert collects Epicor customization definitions and native artifacts into a local backup.
Use the graphical application, the JSON command interface, or the MCP interface.
Each interface uses the same download commands and plugin system.

A collection can include readable source, native packages, resources, and recovery records.
The available content depends on the installed plugins, Epicor capabilities, and the caller's permissions.
Review the outcome for each download. A completed transfer alone does not prove complete coverage.

## Start a download

1. Extract the complete application package into a writable folder.
2. Keep the plugins folder beside the executable.
3. Start Herbert.
4. Open Connection.
5. Enter the Epicor instance address and credentials.
6. Select **Test connection**, then choose your company from the **Company** list.
7. Open Plugins and select the required collections.
8. Open Download and select the destination folder.
9. Select **Preview download**. If the download would change anything on the server, review and authorize those changes.
10. Review the issues and cleanup results when processing ends.

The Epicor account must be an enabled Security Manager.
Native permissions still apply to each operation.
Herbert saves plugin settings beside their modules. The application needs write access to that location.
Plugins ship precompiled, so starting Herbert uses about 1 second of processor time instead of about 2 minutes.

## User workflow

### Enter and save connection information

Open Connection, the first tab, to enter the Epicor server address, company and authentication information. Use the file picker there when your connection information comes from a user-authored environment file.

Use the Epicor instance address, such as https://erp.example.com/Kinetic or https://erp.example.com:8443/KineticTest. Replace the server, optional port and instance name with your environment. You can paste the full browser address. Herbert removes /apps/ERP or /api/v2 and everything after that route, regardless of case, plus query strings and fragments. It preserves the capitalization of your instance path.

Select Save on Connection to keep the selected connection profile. Choose whether credentials come from the current input, your environment file or the operating system secure store. Starting a download does not save newly entered credentials.

Herbert verifies the selected company and caller when it connects. The Epicor user must be an enabled Security Manager. This role check occurs once per explicit connection, not on every read or token renewal. Epicor still enforces permission for each requested operation.

### Test the connection and choose a company

Select **Test connection** to check the address, credentials and account before you download. Herbert signs in, checks the account, lists the companies the account can use, and then signs out. The test collects nothing and changes nothing on the server.

After a successful test, the **Company** field becomes a dropdown. Choose your company from it. If the list could not be read, the field stays a text box and shows why. Enter the company ID yourself in that case. If your saved company is not in the list, Herbert keeps it and warns you.

A company is required. Until you choose one, Herbert shows "Choose a company for this environment. Use Test connection to list the companies your account can use." and **Preview download** stays disabled. Saving a profile without a company is allowed, but Herbert warns you.

If Herbert can't confirm the sign-out after listing companies, it keeps that connection only so it can sign out later. The connection can't be used to download. Disconnecting retries the sign-out.

**Account readiness** shows the result of the last test: **Verified eligible (last check)**, **Verification failed** with the reason, or **Unknown / unverified** before any test. Eligibility does not authorize server changes.

### Allow self-signed certificate

Each connection profile has an **Allow self-signed certificate** option. It is off by default. Turn it on only when the server uses a certificate that Windows does not trust, such as a test server or one that uses your organization's own certificate authority. Herbert shows a warning while the option is on, and screens for that profile show a **Self-signed certificate** badge.

This option relaxes only one check: whether Windows trusts the certificate's issuer. A private certificate authority is accepted. Herbert still checks the server name and the server certificate's dates. The option applies only to that profile.

Limits of this option:

- Any certificate the server presents can serve as the trust anchor, so an expired intermediate certificate is not refused.
- Herbert records the certificate it saw when the download started. If the server presents a different certificate when Herbert reconnects later in the same download, Herbert accepts it without a separate notice. Certificates are not pinned.

Use this option only for servers you control. After a test with the option on, Connection shows "Certificate checks are relaxed for this profile" and the certificate fingerprint, with a **Copy** button for the full value.

A plain `http://` address has no certificate. Connection warns that the connection is not encrypted and that data travels in plain text. Support packets record it as "Not encrypted: plain http, no certificate".

### Error messages

Error notices say what happened and what to do next. Paths, IDs, lower-level errors and server text are under a collapsed **Technical details** section below the notice. Open it when you need to report the problem or look closer. Use **Save diagnostics…** to save a sanitized support packet.

### Set behavior on Download

The tabs are Connection, Download, Plugins and About. Download contains the destination folder picker, output mode, **Preview download** and shared download options. Enter and save connection information on Connection.

Prepare download shows two collection modes: **Direct only** uses existing Epicor services without collection helpers. **Server-assisted** uses native calls and permits a helper when needed or useful. Saved Hybrid and Server side selections map to Server-assisted. Direct only remains Direct only.

Choose the destination and output mode: **New folder for this run** or **Refresh the chosen folder**. Concurrency, timeouts, retries and the fallback page size are under the collapsed **Advanced** section. Choose 2, 4, 6, or 8 **Simultaneous plugins**, with one collection request at a time per plugin. **Preview download** checks what the selected plugins would do, then starts them.

Paged plugins declare editable page settings. UD codes and UD schema use 500 parent rows. BAQs and UBAQs use 100 parent rows. BPM directives, Functions, Classic customizations, and Classic personalizations also use 100. Extended properties uses one complete helper response. These are operating defaults, not total-item limits. Other collectors can use the host fallback page size; Custom resources has no page setting of its own and pages every read, including translations, with it. A plugin can request its next page while it writes received files.

### Choose collections on Plugins

Open Plugins to choose the collections to include. Every installed collection supplies its own settings and help.

A new installation or profile starts with 17 collections selected. These seven start off: Classic customizations, Classic dashboards, Classic personalizations, Configurators, Kinetic personalizations, Programs and extensions, and Solutions and packages. Turn on any you need. Choices you have already saved are kept. An MCP or command-line download uses only the categories it names.

Plugin controls include enablement and collection-specific settings. Category toggles, **Select all available**, **Clear selection** and **Save plugin options** save the current option draft beside that plugin, including filter edits. Opening Plugins does not save options. Downloads use the displayed draft.

Selection filters narrow eligible roots. For most collections an ID prefix is sent to the server as a broad match on its leading characters up to the first wildcard character (`%`, `_`, `[` or `]`), Herbert applies the exact literal, case-sensitive prefix locally, and rows outside the selection are not validated and produce no diagnostics. A few collections cannot narrow the server read and select locally instead: Menus and launch definitions (a selected menu's parents need not match), Searches and navigation, Business rules and mappings, the Report styles and layouts stored-package path, and the Programs and extensions externals list read every eligible row; Kinetic applications reads a light identity-only list and downloads content only for selected applications. Sources with no server filter (Quick, Predictive and context-menu searches, and current stored report packages) are read whole, and rows outside the selection still produce no diagnostics. Each collection's own documentation states its rule. Families or report types you leave unselected are not requested at all. **Clear filter** makes a field blank; save the blank to clear its saved value. Each filter's description states its exact rule and ends with `Matching contract: <id>`. During a download, open **What this download includes** on the "Saving to:" row to inspect the values fixed for that run. Completeness applies within that selection. A successful empty selection reports **No eligible items matched the run selection**. Review incomplete items and failures separately.

### UD codes selection

**Code type ID starts with** uses a literal, case-sensitive prefix. Every entered character, including spaces, is preserved without trimming, wildcard expansion, case folding or Unicode normalization. Blank selects all currently eligible code types under existing company, access and system policies. Herbert reads native GetRows pages in bulk with the broad server match, then selects complete types and every eligible entry locally and validates only the selected types. Empty selected pages still follow native continuation.

MCP discovery uses `pluginSettings:{"ud-codes":{"codeTypeIdPrefix":"EXAMPLE"}}`; `""` explicitly clears the prefix for one run. Omission inherits saved values. Run overrides do not save them; saving a blank with `plugin_configure` clears the saved filter. Exact `UDCodeType` retrieval uses its native `CodeTypeID` identity independently of this discovery filter, while processing options still apply. Matching contract: `ud-codes-code-type-id-prefix-v1`.

The journal retains frozen values, original plugin filter metadata and module fingerprints. Its internal `fullBackup` mode means discovery, not full-environment coverage. Retry/recovery keeps that selection and existing module-hash checks. A legacy journal missing a declared filter stops with `FROZEN_SELECTION_REQUIRED`; restore its original compatible module or start a new explicitly selected run. Refresh preserves unrelated/user-edited files; out-of-selection items are not deletion requests.

### BAQ and Function selection

**BAQs and UBAQs — Query ID starts with** uses `queryIdPrefix` and matching contract `baqs-and-ubaqs-query-id-prefix-v1`. It follows the same literal, case-sensitive and blank/omission rules as UD codes. Each match keeps the complete query, diagrams, required dependency metadata and authored UBAQ method/workflow content. Use filtered discovery with `backup_start`; Exact acquisition through `retrieve_start` is not supported for BAQs and UBAQs.

**Functions — Library ID starts with** uses `libraryIdPrefix` and matching contract `functions-library-id-prefix-v1`. Herbert reads the inventory with the broad server match, selects exact matches locally, then passes only the selected IDs to the existing bulk GetLibraries read. It preserves every Function, signature, reference and company mapping. Empty selected pages still follow native continuation. Exact global `EfxLibrary` retrieval by `LibraryID` ignores this discovery prefix.

Required dependency/reference rows can name IDs outside the root prefix and remain part of the selected artifact. Referenced libraries are not automatically collected. Optional DLL/PDB capture and projects remain separate, default-off settings for selected owners; normal native packages save before optional capture.

Automation and integrations provides Filter sensitive fields and parameters. It is off by default, so Herbert preserves native field and parameter values. Turn it on to omit the plugin's declared sensitive values. System artifact exclusions apply in either mode. Automation and integrations asks Epicor only for the rows the native service admits for your account, restricting each table's read to those row identities, so rows outside your visibility are never downloaded.

Settings marked Demo / example do not affect collection.

### Choose a folder and download

Use the folder picker to choose where Herbert saves the backup, then select **Preview download**. Herbert connects using the selected connection information.

**New folder for this run** creates a folder named Herbert - company - timestamp. **Refresh the chosen folder** updates an existing backup and preserves your edits. Inside the backup, readable files use collection names and artifact names. Herbert Data contains raw evidence and durable recovery records. Conflicts holds incoming versions when an existing local file differs.

Refreshing a backup that an earlier version of Herbert created updates its Herbert Data metadata to the new collection IDs. After that, earlier versions can't refresh that backup, so keep using this version or later for it. Backups this version hasn't refreshed are unchanged.

The **Download monitor** puts concurrent work in one table. Running plugins appear first, followed by queued and finished plugins. The normal window shows all eight possible running plugins together, and the current stage stays visible at the default window size. Each row shows the current operation, the received page's processing progress, completed items, partial items and failures. A plugin remains Running between requests while it processes data. Discovery alone does not mark it Finished.

Host setup and helper cleanup appear above the table. The vacuum summarizes the latest received page from each plugin. It follows processed work rather than elapsed time. Completed-batch totals remain separate from the current pages. **Issues** below the progress track counts the problems; select **Review issues** to read each one, with its saved output, next step and **Technical details**. The action buttons stay in place as statuses change.

The "Saving to:" row shows where this download is saved. A long path is shortened. Hover over it for the full path, or select **Copy** to copy it. The **What this download includes** menu on the same row shows the plugin settings fixed when the download started.

Keep Herbert open. Pause stops new work. Resume continues it. Cancel preserves verified files and finishes cleanup. On Windows, **Open download folder** opens this run's actual output folder once it exists. **New download** becomes available when execution and cleanup finish.

Read the final outcome and any item failures. A permission refusal or partial acquisition means the affected content was not fully collected.

### Read the final results

During a download, items processed counts handled items, including unsuccessful items.
It is not a count of successful downloads. Discovery can add more work while the download runs.

When processing and cleanup finish, the same plugin table shows final states and counts, such as Finished, Incomplete, Failed or Canceled. Each row counts complete, partial and failed items. Review incomplete or failed rows and their issues before starting another download.

| Result | Meaning |
| --- | --- |
| Complete | The item and its readable output completed. |
| Partial | Some content or output is incomplete. The item can still have saved files. |
| Failed | The item did not complete successfully. Read its message under Issues. |
| Absent / not applicable | Herbert verified that the item is absent or does not apply. |
| Excluded | Herbert excluded the item under the collection rules. |

The plugin table also identifies incomplete discovery and items not processed.
**No eligible items matched the run selection** can be a valid result.
A plugin can finish with no files. A missing folder alone does not mean failure.
Use the plugin result and messages to determine what happened.

An ordinary item failure or discovery failure does not stop independent collections.
Herbert records the failure and continues the work it can complete.
Cancellation stops new work. Storage, integrity, or cleanup problems with resources owned by Herbert can stop the shared download.
Wait while the status says **Finishing cleanup**.
Read **Issues** before treating the backup as complete.

### Permissions and temporary access

A Security Manager can still lack permission to read a particular item. Herbert records the refusal when Epicor denies the read.

New downloads use **Direct only** with temporary access off unless you choose otherwise. Earlier saved choices were changed once to these defaults, and the app shows a one-time notice.

Temporary access is a separate choice: **Allow Herbert to temporarily add my Epicor user to required security groups**. When you turn it on and authorize it, Herbert can add an eligible group membership that exact native policy requires, retry the read, and later remove only the membership it added. Existing memberships stay unchanged.

### Review temporary server changes

**Preview download** first asks Herbert what the download would change on the server. If nothing needs authorization, or a saved authorization already covers the same changes, the download starts. Otherwise Herbert shows **Review temporary server changes**.

The review shows the account, company and destination, the collection method and whether temporary access is on. It lists each possible server effect: helpers with their package and SHA-256, group changes, and temporary export files. Direct only never installs a helper, but some native exporters can still create temporary files on the server, so a Direct only download can also need review.

Choose how long the authorization lasts under **Authorization lifetime when a new grant is needed**: **One logical run**, **This connection session** or **Named workflow**. A named workflow needs a **Workflow ID**. It does not create or activate a schedule. Then choose one of these:

- **Authorize these changes and start** saves the authorization and starts the download.
- **Preview Direct without temporary access** switches to Direct only with temporary access off and previews again. It can omit content that needs a helper.
- **Cancel preview** starts nothing.

If the preview has unresolved facts, it can't create an authorization. Use the Direct alternative or fix the cause. If the connection inputs change after the preview, Herbert asks for a new preview. The Download monitor shows which authorization the run uses.

To see your saved session and named-workflow authorizations, select **Saved authorizations…** next to the authorization lifetime options on Download. Each entry shows what it allows and when it was created. Select **Revoke…**, then **Revoke authorization**, to remove one. The CLI and MCP offer the same through `serverChanges_list`, `serverChanges_inspect` and `serverChanges_revoke`. Revoking stops new optional changes under that authorization, including in a download that is running. Cleanup of resources the run owns still runs, and revoking never deletes server resources.

Cleanup warnings remain part of the download result until Herbert reconciles ownership.

### Cleanup and recovery

Herbert records the server helpers, transfer files and temporary permissions it owns so that cleanup can be reconciled. Cleanup removes resources that this run owns. It also removes helpers left on the server by an earlier Herbert version when it recognizes them as released Herbert helpers and no running Herbert is using them. Helpers in use by another run, or that look like Herbert's but can't be confirmed, are left in place and reported. Read any cleanup warnings in the final result.

Keep the run record if cleanup is pending. Do not remove unidentified server files or permissions.

Keep the backup's Herbert Data folder with its downloaded files. It contains the durable records needed for recovery. Recovery through the CLI or MCP requires the original caller and run identity. The graphical interface has no saved-run browser or inspection page.

### Read help

Use the application guide for the Download workflow and plugin help on Plugins for collection details and settings. About contains the application information.

About contains the avatar, vacuum artwork, dedication, and copyright information. The artwork and help documents work offline.

## Included Epicor collections

The standard package selects these 24 Epicor collection plugins.
Teaching examples, including system-info, are excluded. Installed modules determine which collections appear.

Each collection has a stable ID, shown in the table. The CLI, MCP, saved settings and backup metadata use these IDs. Runs, settings and plugins from earlier versions are still read.

| Collection | ID | Content |
| --- | --- | --- |
| UD codes | `ud-codes` | Code types and every code entry with native identity and metadata. |
| UD schema | `ud-schema` | Authored UD fields, tables, relationships, indexes and mappings. |
| Extended properties | `extended-properties` | Authored table and field properties and bindings. |
| BAQs and UBAQs | `baqs-and-ubaqs` | Query definitions, diagrams, update settings, widgets, custom code and local native-format packages. |
| BPM directives | `bpm-directives` | Method and data directives, widgets, code and native packages in separate directories. |
| Functions | `functions` | Libraries, functions, signatures, source variants and references. |
| Classic customizations | `classic-customizations` | Full composite identities, variants, code and resources. |
| Classic personalizations | `classic-personalizations` | Persisted all-user definitions with ownership and scope. |
| Classic dashboards | `classic-dashboards` | Authored trackers, views, query links and deployments. |
| Kinetic applications and layers | `kinetic-applications-and-layers` | Authored layouts, events, rules, drafts and revisions. |
| Kinetic personalizations | `kinetic-personalizations` | All-user personalizations and home-page layouts. |
| Searches and navigation | `searches-and-navigation` | Standalone searches, context links and process links. |
| Menus and launch definitions | `menus-and-launch-definitions` | Custom and demonstrably modified entries and ancestors. |
| Report definitions | `report-definitions` | RDD and BAQ report authored definitions. |
| Report styles and layouts | `report-styles-and-layouts` | Native returned layouts, subreports and resources. |
| Report behavior | `report-behavior` | Routing, breaking, delivery and authored print behavior. |
| Configurators | `configurators` | Retained revisions, rules, methods, lookups and assets. |
| Business rules and mappings | `business-rules-and-mappings` | Authored module rules, mappings and posting rules. |
| Custom resources | `custom-resources` | Original images, scripts, themes, translations and assets. |
| Programs and extensions | `programs-and-extensions` | Exposed binaries, symbols, registrations and SDK metadata. |
| Solutions and packages | `solutions-and-packages` | Membership and retained packages without a new build. |
| Automation and integrations | `automation-and-integrations` | Authored schedules, alerts and integration definitions, with optional sensitive-value filtering. |
| Access and assignments | `access-and-assignments` | Relevant security and assignment configuration. |
| Configuration references | `configuration-references` | Preserve table references from collected definitions. Do not download the referenced business rows. |

Functions collection reads definitions. It does not execute the collected functions.
Solutions collection reads retained packages. It does not build a new solution package.
Configuration references uses previously collected definitions. It does not query the referenced business rows.
Personalization collections can include definitions for other users when the caller has the required access.

The standard package includes compiled BPM, Function and UBAQ support with the matching host and pinned decompiler companion.
Download compiled assemblies and Create decompiled C# projects remain user settings that default off.
Keep the whole backup to retain original compiled bytes under Herbert Data/raw and their journal associations.
Reconstructed source does not replace native recovery packages.

### Collection settings

Each module declares its settings, default values, and help text.
Use the Plugins tab to inspect those declarations for the installed version.
Enablement selects whether a collection participates in a download.
Transfer settings, when present, control that collection's transfer behavior.
Do not treat a transfer chunk size as a maximum artifact size.

Automation and integrations has **Filter sensitive fields and parameters**.
The default is off, which preserves native values.
Enable it to omit the sensitive fields and parameters declared by that plugin.
Only Automation and integrations currently declares this filtering option.
Other plugins do not inherit it. It is not a general secret scanner.
Treat downloaded definitions and native packages as potentially sensitive.

The system-info example starts disabled. Its three demonstration controls save values but do not change its report.

## Technical interface

### One application interface

The GUI, CLI and MCP frontends use the same application commands. Run herbert schema to read command names and input schemas.

The CLI reads one UTF-8 JSON command per line from standard input. A command has schemaVersion 1, a method name and an arguments object. The CLI writes one reply per line.

Use help.read to read these documents and about to read creator details. Neither operation needs an Epicor connection. MCP exposes the same operations as help_read and about.

The public interface performs saved downloads and their lifecycle operations. Discovery occurs within a download. The interface does not provide standalone artifact browsing or example-plugin execution.

The graphical navigation is Connection, Download, Plugins and About, with Connection first. Connection contains environment selection, connection and authentication inputs, the certificate option, the credentials-file picker, Save, Test connection and the company field. Download contains the destination folder picker, output mode, Preview download and shared download options. Pressing Preview download connects with the selected connection information. It does not save newly entered credentials. Plugins contains collection selection and plugin-specific settings.

run.status adds activeOperations, pluginStates and executionActive to the live response. pluginStates identifies admitted plugins as running, completed, failed or canceled, including gaps between requests. Set `summary` to `true` for compact progress with `fileCount` and `fileBytes`. The default response retains the full journal. Active operations and plugin states are temporary status data. Checkpoints do not save them. Continue polling while executionActive is true even if phase is already completed, failed or canceled. Final destination cleanup may still be running. Saved-run reads report false.

### Download requests

Each plugin collects its inventory once, with additional pages when required. It retains full definitions from inventory pages when the service returns them.

A plugin requests each required native export once. Herbert reads the downloaded package locally to create readable code and other views.

A plugin uses separate batched reads for required content that the native export omits. System-marked artifacts remain excluded.

Herbert shares repeated reads within one run. Normal helper calls reuse the ready helper and its definition. Cleanup checks the current server object.

A failed item or inventory branch does not stop independent work. The configured retry options can repeat a failed request.

### Plugin discovery and help

Each Wasm module provides a descriptor with its identity, description, roles, settings and help sections. A help section has a title and an array of plain-text paragraphs.

The host discovers the modules that are present. It has no expected plugin registry. Remove a module and its state files to remove that capability.

The settings interface supplies checkboxes, choices and text filters. The module defines keys, defaults, descriptions and behavior. demoOnly marks an example option that does not affect collection.

Automation and integrations declares filterSensitiveFields, labeled Filter sensitive fields and parameters. Its default is false. Enabling it omits the declared sensitive native fields and parameter values. System artifact exclusions apply independently. Other plugins do not inherit this setting.

Plugins own collection choices: native methods, entity routes and projections, query tables, and native byte-field mappings. The host has no hardcoded collection-method or definition-table allowlists. It validates generic structure, requires actions to match the plugin descriptor, and preserves authentication and ownership controls.

### Portable plugin configuration

Herbert stores a plugin's state beside its module. Configuration and status belong to the plugin. Keep the state file with the module when you move it.

The plugin handles configure requests. The host validates declared value shapes and saves returned opaque state. The host does not interpret a filter or assign collection meaning to a setting. The active download reports runtime status.

The AssemblyScript and Rust system info examples demonstrate the same interface. The AssemblyScript example ships disabled. The Rust source is also available for teaching.

### Plugin loading

Plugins are portable Wasm files. Plugins need no signing key, signature file, native companion, or matching host build.

Herbert validates and compiles the Wasm, checks the plugin ABI and declared capabilities, and reuses compiled modules while their source bytes remain unchanged. The standard plugins ship precompiled, so startup does not wait for compilation.

A plugin whose ID is a retired category code is refused at load with `PLUGIN_ID_RETIRED`.

### Native helpers and temporary access

Herbert installs and removes its server helpers through its authenticated connection, and only with an authorization that covers them. Server leases protect active helpers. Cleanup removes only resources that this run owns, checking the Herbert marker and the exact native generation.

The host records permission checks, temporary group changes, helper upload, use and removal, and permission restoration. A successful operation or a record marked removed does not replace independent readback of server cleanup.

Connection admission requires an enabled Security Manager. Herbert checks the role once per explicit connection. Reads and token renewal do not repeat admission. Native operation permissions remain enforced by Epicor.

Temporary permission grants still require evidence that distinguishes Herbert's additions from pre-existing access. Preserve unrelated group memberships. An uncertain write needs fresh readback before a retry.

### Transfer and cleanup evidence

Readable output uses collection display names and artifact names. If a journal lacks a collection display name, output naming falls back to its internal category ID. Herbert Data retains raw files, manifests, journals and checkpoints. Conflicts preserves incoming versions when an existing local file differs.

Resuming a download uses its durable run identity and original caller. Pending cleanup remains visible until independent readback proves it is complete. `run_status` reports server-change outcomes under `serverChanges`, including `leaseLost`, `resources` and `foreignPending`.

## Command-line use

Start the graphical application with `herbert` or `herbert gui`.
Run `herbert --version` to read the application version.
Run `herbert schema` to get the current operation schemas without connecting to Epicor.

```text
herbert cli
```

Write one JSON command per input line. For example:

```json
{"schemaVersion":1,"method":"capabilities","arguments":{}}
```

Read one JSON reply per output line. Use the generated schemas for operation arguments.
Keep credentials out of command arguments and shell history.
Connection inputs support inline credentials, a user-authored environment file, or the operating system secure store.
Herbert reads an environment file without changing it. The file itself is not encrypted by Herbert.

Supported startup options are `--state-dir PATH`, `--plugin-dir PATH`, and `--coordination-dir PATH`.
The default plugin directory is beside the executable.
Herbert stores non-credential state in the `data` folder beside its executable.
Move this folder with Herbert to retain profiles, settings, and recovery records.
Saved credentials remain in the operating system secure store. They do not travel
with the application folder. The default coordination folder is `data/coordination`.
An explicit `--coordination-dir` can still share local access-grant coordination.

Keep credential values out of that directory.

Herbert records temporary helper activity on the server and renews the run's lease
every 30 seconds. If the lease can't be renewed, Herbert stops new helper use and
continues Direct reads. It does not cancel the whole download, and cleanup still runs.
At the end of a download, cleanup removes only the helpers and files that this run owns.
Other runs' helpers, and helpers Herbert can't attribute, are left in place and
reported as pending for another owner. An older helper without a recorded
authorization is removed automatically only when a local run record names its exact
generation for the same account, endpoint and company. Direct only never installs a helper.

A `userFile` credential source selects its named environment with `environmentId`.
The older `environment_id` spelling is still accepted.

### MCP use

Configure an MCP client to start `herbert mcp` as a local stdio server.
Run `herbert mcp-config --client <client>` to print a configuration with this installation's actual paths. The clients are `codex`, `claude-code`, `claude-desktop` (the default), `gemini`, `vscode`, `cursor` and `generic`. It does not load state or connect to Epicor. "Connect your AI client" in the [MCP guide](MCP-GUIDE.md#connect-your-ai-client) says where each client keeps its configuration and how to check that Herbert's tools are listed.
The package includes a Herbert skill for Claude Code in `skills/herbert` and for Codex in `skills/codex/herbert`. Copy the Codex one to `~/.codex/skills/herbert`.
The package also includes `mcp.claude-desktop.example.json` and `AGENTS.md`, the operating instructions for agents. MCP clients can read the same instructions through `help_read` as `agent-instructions`.
The client sends the standard initialization request before calling tools.
Operation names use underscores in MCP, such as `help_read` for `help.read`.
The CLI and MCP interfaces share command behavior and validation.
Requests use the collection IDs listed above. A retired category code is refused with `LEGACY_CATEGORY_ID`, and `error.context.replacementCategory` names the current ID.
`connection_test` without a company returns `availableCompanies`. A download without a company is refused with `COMPANY_REQUIRED`, and `profiles_upsert` without a company returns the warning `COMPANY_NOT_SET`.
Before a download that changes the server, call `serverChanges_preview`, then `serverChanges_authorize`, and pass the returned authorization ID as `authorizationId` in the start request.
Errors keep their codes. Details are in the diagnostic's `context`, and the error text adds a next step when the message has none.
Poll run status until `executionActive` is false before treating cleanup as finished.
Read the [MCP guide](MCP-GUIDE.md) for the complete protocol, tool, result, effect, and recovery reference.

## Architecture and plugin development

The application owns authentication, command dispatch, transfer storage, and cleanup records.
The contract crate defines messages between the host and modules.
The collection package defines Epicor collection behavior.
Portable WebAssembly modules declare their capabilities and request host actions through that contract.

The repository contains Rust and AssemblyScript teaching examples in `examples/system-info`.
The examples do not ship in the standard package.
Both examples request information about the computer that runs Herbert.
They do not inspect the Epicor server.
Read the example README for its byte bridge, configuration state, and build commands.

Implement the descriptor, configuration, and action responses defined by the supplied contract.
Declare settings and help text with the module.
Keep configuration semantics in the module, and request only the host capabilities it declares.
Build a Wasm file and place it in the selected plugin directory.
Use the example test scripts to check ABI behavior and host integration.
Test with authenticated Epicor access separately when a plugin reads native content.

## Build from source

The Windows package build requires Rust 1.99 or later and Python 3.12 or later.
Install MSVC C++ build tools and the Windows SDK resource compiler, `rc.exe`.
Set `HERBERT_RC` when the resource compiler requires an explicit path.
Installed Epicor client DLLs are not required.

Run these commands from the repository root:

```text
rustup target add wasm32-unknown-unknown
python scripts/build_workspace.py
python scripts/build.py --managed-decompilation-companions <companion-root>
```

Supply a pinned companion root containing the target RID folder, companion.json and the native executable.
The builder rejects a missing or invalid companion instead of producing a reduced package.
Use `--output <new-directory>` to write the package to a folder you choose.
Set `HERBERT_BUILD_HOME` to choose the build cache folder. The default is `$XDG_CACHE_HOME/herbert` or `~/.cache/herbert`.
Cargo defaults to PATH. Use `--cargo` to select an explicit executable when needed.
The builder packages the host, portable modules, and dependency notices.
It also packages the README, this guide and the MCP guide.
Preserve the supplied third-party notices with the distribution.

## Troubleshooting

| Symptom | Action |
| --- | --- |
| Connection admission fails | Select **Test connection**, then check the instance address, company, credentials, and enabled Security Manager status. Open **Technical details** under the message for the server's reply. |
| A certificate error appears | Check the server address and the server certificate's name and dates. Use **Allow self-signed certificate** only for a server you control that uses a certificate Windows does not trust. |
| **Preview download** is disabled | Choose a company. Use **Test connection** to list the companies the account can use. |
| Epicor refuses a collection read | Review the exact item failure and native permission. Security Manager admission does not grant every read. |
| A collection is missing | Check the plugin directory and module diagnostics. Herbert discovers only present modules. |
| Settings cannot be saved | Check write access beside the module. Keep its state file with it when moving the module. |
| A run ended but remains active | Continue polling. Destination cleanup can continue after the phase changes. |
| Cleanup is pending | Keep the run records and use recovery with the original caller and run identity. |
| A local file differs | Inspect Conflicts for the incoming version. Preserve both versions until reviewed. |
| A download is incomplete | Read item failures and retained evidence before deciding whether to retry. |

Do not remove unidentified server resources to clear a warning.
Do not repeat an uncertain write without checking the current native state.

## What a backup proves

A backup preserves what the server returned. An export does not prove that an import, report rendering, or business operation will succeed.
For example, an RDL can contain Visual Basic code in its Code element.
Preserving that code proves byte preservation. It does not prove that SSRS executed the code or rendered the report.
Evaluate restore and application behavior in an appropriate target environment before relying on a backup operationally.

## Known limits

Herbert runs on Windows.

These cases have not been fully tested against real Epicor data:

- Business rules and mappings: some posting-rule child records.
- Access and assignments: custom permission rules that control who can use an installed Epicor service.
- Programs and extensions: populated external-program catalogs and some extension contexts.
- Searches and navigation: some physical context-menu records and search combinations owned by other users.
- Menus and launch definitions: populated global custom menus.
- Kinetic applications and personalizations: some home-option and linked-base paths.
- Classic definitions: some populated legacy and compressed definitions.
- Reports: deployed SSRS acquisition, rendering, and code execution. Stored report packages and embedded code have separate preservation tests.

Report styles have two restore limits in Epicor's importer:

- Epicor imports at most one style rule per report style. A style with more than one rule is saved but reported as partial with `REPORT_STYLE_MULTIPLE_RULES`. Before importing, keep one rule in the saved style XML, then add the others again in Epicor. If an import was already refused, remove the partial report record it left.
- Epicor's importer ignores a style's CompanyList. When a style has one, Herbert saves a `<style>-RESTORE.txt` note beside the style XML with the saved value. Set CompanyList again after the import.

The `reports.zip` that Herbert builds holds report paths relative to the server's reports folder, as Epicor's report import expects.

An export interrupted by a lost connection can have an uncertain server result. The inspected native interfaces do not provide an operation-specific completion receipt. Herbert retains recovery information instead of assuming success, repeating the export, or deleting a file that might still be in use.

Extended properties can contain values whose authoring history is unknown. Herbert preserves those values. Their presence does not prove that a person authored each value.

Herbert uses native System markers to select Kinetic files. It preserves eligible source without a separate application-owner lookup.

A backup does not prove that import, restore, ordinary-user access, or business behavior will work. Check the final result for each plugin and validate restore in the intended environment.

## Licensing and notices

Herbert uses the [Herbert Source-Available License 1.1](../LICENSE.md).
Personal and internal business use, modifications, and free sharing are allowed.
You may charge for installation, support, and customization.
Selling Herbert or providing it as a paid hosted service requires a separate written agreement.
Third-party components retain their own licenses and notices.
The distribution includes the dependency inventory and preserved notice text.

## Contributions

Kevin Lincecum maintains the official project.
The project does not accept pull requests or outside code contributions.
