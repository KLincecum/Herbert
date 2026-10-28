---
name: herbert
description: Operate Herbert through MCP to collect Epicor definitions and artifacts, retrieve exact items, inspect results, and recover runs. Reconcile owned cleanup or decompile local assemblies. Use for Herbert workflows and troubleshooting, not general Epicor editing or deployment.
---

# Herbert

Herbert is a local Windows application that collects Epicor definitions and related files.
Installed plugins determine its available categories, settings and output.
Use the existing Herbert MCP connection when available.

## Discover the host and tools

Call `tools/list` through the MCP client.
Read the current input and output schemas before constructing requests.
MCP names replace application dots with underscores and retain letter case.
For example, `run.retryFailed` becomes `run_retryFailed`.

Call `about` for the installed version and `capabilities` for categories, target identities, settings, help and diagnostics.
Call `settings_inspect` and `collection_preferences_get` for effective choices.
Use `help_read` for embedded guidance when local documents are unavailable.
The `schema` tool returns the application catalog with dotted operation names.

If the client has no Herbert MCP server, locate the complete Herbert package.
Configure the client to run its absolute executable path with `mcp` as the first argument.
Keep one persistent host for the connection workflow.
Use explicit absolute `--state-dir`, `--plugin-dir` and `--coordination-dir` paths when selecting existing state.
Otherwise, state and plugins default to folders beside the executable.
Coordination defaults to a folder under state.

A direct stdio client uses protocol `2025-11-25`.
Send `initialize`, then `notifications/initialized`, then `tools/list`.
Send one JSON-RPC message per physical line and keep logs off standard output.
A normal MCP client handles that handshake.
Read the [MCP guide](../../docs/MCP-GUIDE.md#start-the-server) for client configuration and framing.

## Select scope and connect

Use the supplied endpoint, company, caller, categories and destination.
Resolve missing scope from existing profile metadata or ask the user.
Do not infer authority to change a server from a profile name or a successful login.
The Epicor caller must be an enabled Security Manager. Native operation permissions still apply.

Use `profiles_list` to inspect saved profiles without secrets.
For saved credentials, select `profileId` and `credentialSource:{"type":"secureStore"}`.
For a user-authored file, use this credential source:

```json
{"type":"userFile","path":"C:/Private/environments.json","environment_id":"authorized-lab"}
```

Select the exact environment ID. Herbert does not change that file.
The `environment_id` key in this credential source uses an underscore.
Herbert also accepts inline credentials through stdin. Follow the actual schema.
Keep secrets out of shell arguments, logs, source control and collected output.
Use `credentials_status` to check saved credential presence.
Use `credentials_save` only when the user wants the endpoint-bound credential stored.

Call `connection_test` with the selected connection inputs to test login and logout.
Call `connection_connect` to open the connection for collection.
For example, these are tool arguments for an existing authorized profile:

```json
{"profileId":"authorized-lab","company":"LAB","credentialSource":{"type":"secureStore"}}
```

Check the returned endpoint, company, caller, auth mode and admission.
Use `companies_list` only through the active authorized connection.

If admission fails and `error.context.connectionRetained` is true, preserve both the primary error and cleanup diagnostic.
Call `connection_disconnect` in that same host.
Another process cannot clean up its in-memory session.
Inspect a repeated cleanup failure before retrying.
Failed admission does not authorize collection.

## Choose collection behavior

Read installed category help before selecting settings.
Use `plugin_configure` for persistent declared options beside a module.
Use `pluginSettings` for supported run overrides.
Do not invent keys or copy another category's settings.

Omitted run values inherit saved plugin values. Explicit blank filter overrides clear that filter for one run without saving. Saving a blank with `plugin_configure` clears the saved value. GUI category toggles, Select all available, Clear selection and Save plugin options save the current option draft too. Never infer that a filter is inactive by comparing with descriptor defaults: saved values become those defaults.

C01's declared `codeTypeIdPrefix` is a literal, case-sensitive prefix, preserving spaces and all entered characters without wildcard expansion or normalization. Use `"pluginSettings":{"C01":{"codeTypeIdPrefix":"EXAMPLE"}}` for selected discovery, or an explicit `""` to clear it for that run. Matching contract: `c01-code-type-id-prefix-v1`. The collector reads/validates native pages in bulk, then emits complete selected types and entries locally. Exact `UDCodeType` retrieval ignores this discovery filter.

C04 declares `queryIdPrefix` (**Query ID starts with**, `c04-query-id-prefix-v1`); C08 declares `libraryIdPrefix` (**Library ID starts with**, `c08-library-id-prefix-v1`). They use the same literal case-sensitive rule, preserving all characters, and the same blank/omission/save behavior. Use their declared keys under `pluginSettings.C04` or `pluginSettings.C08`. Keep whole queries/libraries and required dependencies; empty selected pages retain native continuation. Optional compiled settings apply only to selected owners and remain separate from root selection.

C04 `retrieve_start` acquisition is unsupported: use its prefix with paged `backup_start`. C08 exact global `EfxLibrary` retrieval by `LibraryID` ignores the discovery prefix. Its complete Function/signature/reference/company-mapping graph remains intact; referenced libraries are not automatically collected.

For a read-only check, explicitly set `collectionMethod:"directOnly"` and `temporaryAccess:false`, select one plugin, and use a small existing selection. Do not inherit server-changing defaults for that check.

Direct only uses existing services without installing collection helpers.
Server Assisted permits temporary helpers where required or useful.
Temporary access can add owned permissions under native policy.
Fresh defaults enable Server Assisted and temporary access.
Set `collectionMethod` and `temporaryAccess` explicitly to match the authorized task.
A collection request does not authorize unrelated native changes.
This skill grants no authority to change an Epicor server.

Use declared page sizes and effective host settings.
They are operating choices, not total-item limits.
If changing host behavior, read `settings_inspect` first and preserve unrelated fields in the complete `settings_update` object.
Read back effective settings. See the [settings guidance](../../docs/MCP-GUIDE.md#defaults-and-operating-choices) for persistence and timing.

## Collect and inspect

Use `backup_start` for discovery within selected categories and effective plugin options.
Choose category IDs from `capabilities`. Explicit unavailable categories cause rejection.
This example uses an available C02 category and an example destination:

```json
{
  "requestId": "schema-001",
  "destination": "C:/Backups/Herbert",
  "outputMode": "separate",
  "categories": ["C02"],
  "collectionMethod": "directOnly",
  "temporaryAccess": false
}
```

Use `separate` for a new run folder beneath the destination.
Use `refresh` to refresh the chosen destination with conflict preservation.
Inspect the resolved destination returned by the host.

Keep one stable `requestId` for one logical start request.
The same request and scope return the existing run.
Herbert rejects changed scope with the same ID.
After a lost response, inspect `run_list` and the saved journal before starting with a new ID.

Use `structuredContent` as the authoritative result.
The common envelope has `schemaVersion`, `ok`, `result` and `error`.
On failure, inspect the stable error code, message, retry guidance and safe context.
MCP also sets `isError:true`.

An accepted start returns `runId`. It does not establish completion.
Poll `run_status` with:

```json
{"id":"<returned-runId>","summary":true}
```

Continue until `executionActive` is false, even when the phase already says completed, failed or canceled.
Then call `run_status` without `summary` and inspect the full journal.
Check artifacts, discovery, partial or failed items, diagnostics and cleanup.
An empty file list alone does not prove that the requested content is absent.
Report actual outcomes against the requested scope. `scope:fullBackup` is the persisted discovery mode, not proof of full-environment coverage. Inspect `pluginSettings`, `pluginSelectionContracts`, `pluginHashes` and actual identities. Completed overall discovery with no selected roots yields a successful empty-selection message in `discoveryMessages`; distinguish that from invalid settings, missing exact identities and denied/failed reads. Empty intermediate pages can still continue. Preserve existing files outside a selected refresh scope.

Keep the complete backup, including `Herbert Data`, raw bytes and journals.
Preserve changed local files and incoming versions in `Conflicts` until you resolve their differences.
Use `diagnostics_export` with a new ZIP path for a sanitized support packet when needed.
After network work and cleanup stop, call `connection_disconnect` in the owning host.

## Retrieve an exact item

Use `retrieve_start` with a stable request ID, destination, output mode and `identities` only where the installed plugin supports acquisition. C04 requires filtered paged discovery instead.
Read each category, kind, scope and native key from installed capabilities and reliable source evidence.
Do not guess identity from a filename.
For a known C02 schema, the identity shape is:

```json
{"category":"C02","kind":"UDTableSchema","scope":{"type":"global"},"keys":{"SystemCode":"Ice","DataTableID":"Example_UD"}}
```

Replace the example keys with verified values and put this object in the `identities` array.
Use the same explicit collection options and monitoring workflow as a backup.
Some exact routes still acquire bulk context.
Read the [guide](../../docs/Herbert-Guide.md) for the collections and their output.

## Handle interruption and partial results

| Tool | Correct use |
| --- | --- |
| `run_pause` | Stop new dispatch. Active work can finish. |
| `run_resume` | Continue a paused active run. |
| `run_cancel` | Request cancellation, then poll until execution stops. |
| `run_recover` | Resume an interrupted network run. |
| `run_retryFailed` | Acquire failed or incomplete content again. |
| `run_reprocess` | Process saved local material without an Epicor connection. |

These controls take the saved run `id`.
For network recovery, use the owning state and coordination directories and reconnect with the original endpoint, company and caller.
Resolve reported ownership or cleanup problems, then poll the recovered run.
Keep successful output and continue independent work when one collection is unavailable. Retry/recovery retains frozen settings and module-hash checks. A legacy journal missing a filter stops with `FROZEN_SELECTION_REQUIRED`; restore its original compatible module or start a new explicitly selected run. Never insert today's saved filter into that journal. Exact IDs and filter metadata are private evidence: share sanitized diagnostics, not raw journals.

Do not replay an uncertain native action merely because a response was lost.
If `SERVER_COLLECTION_UNRESOLVED` appears, call `serverCollection_inspect`.
Independently establish that the exact server operation finished or stopped.
Then call `serverCollection_reconcile` with its `invocationId`, `confirmedServerStopped:true` and observed `evidence`.
This records an assertion locally. It does not stop server work.
A client timeout, cancellation or exit is not stop evidence.

Use `helpers_reconcile` with the exact helper operation `id` from helper evidence.
That ID differs from the run ID and invocation ID.
The operation can remove owned native resources and requires its original caller and an inactive operation.
Respect ownership and source checks. Preserve unrelated helpers, files and memberships.
Read [server collection recovery](../../docs/MCP-GUIDE.md#server-collection-recovery) for this exceptional workflow.

## Route local decompilation

The standard package includes compiled BPM, Function and UBAQ support and the managed decompiler companion.
Capture and project creation remain user settings that default off.
Check installed capabilities before use.
Local assembly decompilation does not need an Epicor connection.
Use `decompiler_start` with absolute `assembly` and `output` paths. The output folder must be new.
Supplying `symbols` requires `symbolMode:"externalPortable"`.
The default symbol mode is `none`.

The accepted result has `jobId`, not a collection run ID.
Use that ID with `decompiler_status`, `decompiler_cancel` or `decompiler_recover`.
Inspect final job state, diagnostics and output evidence.
Recovery uses verified snapshots and exclusive output ownership.
Preserve job records and output bookkeeping.

## Interpret collected output

Native packages, original resources, readable source and definition history have different uses.
Do not call every JSON file or reconstructed project directly restorable.
Use applicable supported native import paths only within the user's destination authority.
Where Epicor offers no importer for a collected graph, preserve accurate definitions and history.
Do not require an invented recovery writer for ordinary collection.
Consult the relevant shipped category guide for recovery details.
