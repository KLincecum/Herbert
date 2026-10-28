# Herbert MCP guide

This guide is for an MCP client that has no prior Herbert context.

Herbert is a local Windows host. It collects authorized Epicor definitions and related files. It can also decompile a local managed assembly.

## Start the server

Run `herbert mcp` as a local stdio server. Send one JSON-RPC message on each input line. Read one response from each output line.

Do not write logs to standard output. Extra output would damage the JSON-RPC stream.

Herbert supports MCP protocol version `2025-11-25`. Start with `initialize`. Then send the `notifications/initialized` notification. Call `tools/list` before tool use.

Use a string or integer request ID. Herbert rejects null, Boolean, object, and fractional IDs. A notification has no `id` member. Herbert does not run tools from notifications.

MCP tool names use an underscore where application operation names use a dot. Letter case does not change. For example, `run.retryFailed` becomes `run_retryFailed`.

Use an absolute executable path in the client's stdio server configuration. For example:

```json
{
  "mcpServers": {
    "herbert": {
      "command": "C:/Apps/Herbert/herbert.exe",
      "args": ["mcp", "--state-dir", "C:/HerbertState", "--plugin-dir", "C:/Apps/Herbert/plugins", "--coordination-dir", "C:/HerbertCoordination"]
    }
  }
}
```

Adapt the outer configuration keys to the client. Keep credentials out of command arguments and configuration logs.

The mode must precede the path flags. Each flag requires a directory path:

| Flag | Omitted value | Effect |
| --- | --- | --- |
| `--state-dir` | `data` beside the executable | Stores profiles, preferences, run indexes, and decompiler job records. |
| `--plugin-dir` | `plugins` beside the executable | Selects installed modules and their portable options. |
| `--coordination-dir` | `coordination` under the selected state directory | Stores access ownership and server collection coordination. |

Explicit relative paths resolve from the process working directory. Use absolute paths to avoid client-dependent locations.
Hosts that must coordinate access must use the same coordination directory. This is local coordination, not a distributed server lock.
Start one persistent host for a connection workflow. A different process cannot recover an in-memory session from this host.
Closing standard input starts host shutdown. Shutdown cancels active work, waits for cleanup, and attempts logout. Inspect standard error for shutdown diagnostics.

## Read the contracts

Each `tools/list` entry has these fields:

- `inputSchema` defines the accepted arguments.
- `outputSchema` defines `structuredContent` for success and failure.
- `description` explains the result, effects, and next action.
- `annotations` identify read-only operations.

The `schema` tool returns the same application catalog. Its operation names use dots.

Each tool call returns a common envelope in `structuredContent`:

```json
{
  "schemaVersion": 1,
  "ok": true,
  "result": {},
  "error": null
}
```

On failure, `ok` is false and `isError` is true. `result` is null. `error` contains a stable code, a message, retry guidance, and safe context.

Use `structuredContent` as the authoritative result. The text content is the same JSON for clients that only read text.

## Safe workflow

1. Call `capabilities` to discover installed categories and their availability.
2. Read `settings_inspect` and `collection_preferences_get`.
3. Configure plugin options only when the discovered schema requires them.
4. Create or select a profile. Save credentials only when local storage is acceptable.
5. Call `connection_test` to test the exact endpoint, company, caller, and authentication mode.
6. Call `connection_connect` before an Epicor collection.
7. Call `backup_start` or `retrieve_start` with a stable `requestId`.
8. Poll `run_status` until `executionActive` is false.
9. Inspect the final phase, diagnostics, artifacts, and completeness evidence.
10. Call `connection_disconnect` after all network runs stop.

Do not treat an accepted run as a completed run. Do not treat a completed phase as final while `executionActive` is true.

## Defaults and operating choices

A fresh host uses these initial values before saved preferences or explicit settings apply:

- Collection method is `serverAssisted`.
- Host concurrency is 4.
- Server concurrency is 4.
- Local decompiler workers are 4.
- Native page size is 500.
- Temporary access is enabled.
- Output mode must be `refresh` or `separate`.
- Retry count is 0 unless settings specify another value.
- Connection, service, export, and inactivity timeout values start as null.
- `minimumRequestIntervalMs` and `retryBackoffMs` start as null.

These values are operating defaults. They are not general workload limits. Read `settings_inspect` for the effective values and timing guidance.

The saved collection preference overrides the fresh `serverAssisted` default. Explicit run `collectionMethod` overrides that preference.
Saved host `localDecompilerWorkers` overrides 4. A legacy Functions preference can supply the initial saved value during migration.
Profiles and environment files can supply company and concurrency defaults. GUI preferences are separate from MCP host settings.
Read `settings_inspect` after configuration. Read `capabilities` for plugin-specific page sizes and saved options.
The host page size does not replace every plugin's own paging choice.

`settings_update` persists `localDecompilerWorkers`. Other operating settings remain in the current host.
Plugin concurrency applies to new runs. Request pacing applies to new dispatch.
Server concurrency and local worker changes apply to new runs. Raw decompiler worker changes wait until the current batch is idle.
Connection timers and sizes apply to new connections. Recovery retains the saved run scope and settings.

Connection requests validate settings and credentials before changing shared scheduling.
A valid request applies its concurrency and pacing before authentication. Explicit connection settings also update server concurrency for new runs.
Authentication or admission can then fail without undoing those valid settings. Rejected invalid input does not apply them.
Connection settings do not persist all host preferences. Use `settings_update` for the host worker preference.

## Credentials and local state

Prefer `secureStore` for saved credentials. `credentials_save` binds a credential to the profile endpoint. It never returns the secret.

Inline credentials travel through local MCP standard input. Keep client logs and process tracing free of secrets.

A `userFile` source reads a user-authored JSON file. Herbert does not edit that file. Keep the file outside source control and download folders.

Profiles and host preferences use the selected state directory. Plugin options live beside each module in its portable state file.
Run output folders contain collected files and recovery journals. Coordination records use the selected coordination directory.

## Side effects and native changes

Tool annotations are hints. Read the stated effects before use.

On first use, `settings_inspect` can create `decompiler-preferences.json`. It can migrate the legacy Functions worker choice into that file.
After Herbert saves the host preference, migration removes only the legacy worker field from plugin state. Existing host preferences take precedence.
Other initialization paths include `settings_update`, `plugin_configure`, `retrieve_start`, `decompiler_start`, and `decompiler_recover`.

`backup_start` and `retrieve_start` can use temporary native access. Server-assisted plugins can create temporary native helpers. Herbert records ownership and cleanup evidence.

`helpers_reconcile` can remove owned temporary native resources. Use the exact helper operation ID. The original caller and source checks must pass.

`serverCollection_reconcile` changes local coordination state after you supply independent server stop evidence. It does not stop a server process.

## Discovery filters, exact retrieval and persistence

Read the installed category settings and help from `capabilities` before constructing `pluginSettings`. The plugin defines the match rule. `backup_start` discovers roots within those effective options; `retrieve_start` selects exact native identities independently of discovery filters where the plugin supports acquisition. Processing options still apply. C04 currently supports paged discovery only; use its Query ID prefix with `backup_start`.

Omitted run values inherit the plugin's saved values. A fresh plugin with no saved filter discovers all currently eligible roots within its existing company, tenant, ownership, system and access policies. An explicit blank filter override clears that filter for one run. Run overrides do not save options. `plugin_configure` saves declared values; saving a blank clears the saved filter. In the GUI, category toggles, Select all available, Clear selection and Save plugin options also save the current option draft. Opening the GUI does not save it.

C01 declares `codeTypeIdPrefix`: **Code type ID starts with**. It is a literal, case-sensitive prefix. Every entered character, including spaces, is preserved; there is no trimming, case folding, wildcard expansion or Unicode normalization. The matching contract is `c01-code-type-id-prefix-v1`. Herbert reads native GetRows pages in bulk, validates each complete page, then selects complete code types with all eligible entries locally. Native paging continues through empty selected pages. A filter narrows output; it does not promise narrower server reads.

For an authorized C01 discovery, add `"pluginSettings":{"C01":{"codeTypeIdPrefix":"EXAMPLE"}}` to the start arguments below. To clear a saved prefix for that run, send `"pluginSettings":{"C01":{"codeTypeIdPrefix":""}}`. Exact `UDCodeType` retrieval uses its native `CodeTypeID` identity and ignores this discovery prefix. Do not invent another ID-list syntax.

C04 and C08 use the same literal prefix rule and blank/omission persistence behavior:

| Category | Setting / label | Matching contract | Complete selected unit |
| --- | --- | --- | --- |
| C04 | `queryIdPrefix` / Query ID starts with | `c04-query-id-prefix-v1` | Whole BAQ or UBAQ, diagrams, required dependency metadata and authored update methods/workflows |
| C08 | `libraryIdPrefix` / Library ID starts with | `c08-library-id-prefix-v1` | Whole global library, all Functions, signatures, references and company mappings |

For C04 discovery, use `"pluginSettings":{"C04":{"queryIdPrefix":"EXAMPLE"}}`. C04 exact acquisition through `retrieve_start` is unsupported. For C08 discovery, use `"pluginSettings":{"C08":{"libraryIdPrefix":"EXAMPLE"}}`. Exact C08 `EfxLibrary` retrieval uses global scope and `LibraryID`, independently of the discovery prefix. Company mappings remain library content. C08 selects validated inventory IDs before its existing bulk GetLibraries read. Both collectors retain native continuation through empty selected pages. Required dependency/reference rows can name IDs outside a prefix; they remain with the selected artifact. Referenced libraries are not automatically collected.

Optional compiled capture and projects remain separate settings, default off, and apply to selected owners. Normal native packages save independently before optional capture. Direct only retains normal output and reports any requested helper-dependent gap.

The journal's `scope:fullBackup` is the persisted discovery execution mode. It does not assert full-environment coverage. Inspect frozen `pluginSettings`, `pluginSelectionContracts` (original plugin-declared filter descriptions and matching version), `pluginHashes`, discovered identities, category outcomes and artifact completeness. Compact status also carries those frozen options, metadata and fingerprints. The GUI shows them under **Run scope and frozen options**. Do not infer inactive filters by comparing values with descriptor defaults: saved values become those defaults.

A completed discovery with `discoveryFinished[category]:true` and `categoryOutcomes[category]:verifiedAbsent` is a successful empty selection: no eligible items matched the run selection. `run_status.discoveryMessages` presents that message only after committed overall discovery is complete and its total selected roots are empty. It does not establish that the environment has no artifacts outside that selection. An empty intermediate page is not completion. Invalid setting types, missing exact identities and failed/denied reads remain separate failures; an invalid filter never broadens into unfiltered collection.

Recovery and retry use frozen values. A changed selection starts a new logical request. Existing plugin-hash checks still apply. If an older journal omits a declared filter, this host refuses guest dispatch with `FROZEN_SELECTION_REQUIRED` rather than applying today's saved filter. Restore the original compatible module or start a new run with an explicit selection. Saved output remains preserved. Refresh preserves unrelated and user-edited files; an item outside the selected scope is not a deletion request.

For a read-only check, explicitly send `"collectionMethod":"directOnly","temporaryAccess":false`, select one plugin and the smallest useful existing selection. Default Server Assisted and temporary access settings do not establish mutation authority.

## Request identity and replay

Use one stable `requestId` for one logical `backup_start` or `retrieve_start` request. The same request and scope return the existing run. Herbert rejects a changed scope with the same ID.

Do not replay a failed native write only because a transport response was missing. First inspect the journal and native state. Use the reported reconciliation path.

Herbert does not promise that every native action is idempotent. Error context states when cleanup or connection state is uncertain.

## Pause, cancel, retry, and recovery

`run_pause` stops new dispatch. Work already in progress can finish. `run_resume` continues dispatch.

`run_cancel` requests cancellation. Poll `run_status` until `executionActive` is false. Verified files and journals remain.

`run_recover` resumes an interrupted network run. It requires the original endpoint, company, caller, and access reconciliation.

`run_retryFailed` can acquire server data again. Use it for failed or incomplete acquisition.

`run_reprocess` uses saved local material. It does not require an Epicor connection.

Raw decompiler jobs use separate `decompiler_*` controls. Their job IDs and output ownership rules do not change.

## Paging and progress

Public MCP tools do not stream partial tool results. Poll status tools for progress.

Herbert performs native paging inside a collection. The `pageSize` input selects the requested native page size. It is not a result limit.

`run_list` is a local index. It is not a completeness report. Use `run_status` for the saved journal and artifact evidence.

The stdio protocol uses newline-delimited JSON. One JSON value must remain on one physical line.

## Tool reference

The schemas are the exact machine contracts. The following table explains intent, result, and effects.

| Tool | Main input | Result and effects |
| --- | --- | --- |
| `decompiler_start` | Assembly path, new output folder, and optional portable symbols | Accepts a local job. It snapshots input and writes a project tree. Poll `decompiler_status`. |
| `decompiler_status` | `id` | Returns saved job state, files, hashes, diagnostics, and live statistics. It is read-only. |
| `decompiler_cancel` | `id` | Requests cancellation. Poll `decompiler_status` for the final state. |
| `decompiler_recover` | `id` | Resumes from verified snapshots. It requires exclusive ownership of the output folder. |
| `collection_preferences_get` | None | Returns the saved default collection method. It is read-only. |
| `collection_preferences_set` | `collectionMethod` | Saves `directOnly` or `serverAssisted` for future runs. |
| `schema` | None | Returns every application operation and its schemas. It is read-only. |
| `help_read` | None | Returns embedded user, technical, and MCP guides. It is read-only. |
| `about` | None | Returns product identity, version, credits, and public links. It is read-only. |
| `helpers_reconcile` | Exact helper operation `id` | Reconciles owned native resources. This can change the authorized server. |
| `plugin_configure` | Category, optional selection, and declared setting values | Saves portable plugin state. It does not contact Epicor. |
| `profiles_list` | None | Returns local profiles without credential values. It is read-only. |
| `profiles_upsert` | Profile ID, endpoint, authentication mode, defaults, and optional file reference | Creates or replaces one local profile. Credentials remain separate. |
| `profiles_rename` | Profile ID and name | Changes only the display name. |
| `profiles_remove` | Profile ID | Removes the profile. Remove its secure credential separately. |
| `profiles_associateFile` | Profile ID, file path, and exact environment ID | Saves a file reference. The user file remains unchanged. |
| `profiles_removeFile` | Profile ID | Removes the saved file reference. The user file remains unchanged. |
| `credentials_status` | Profile ID | Reports secure credential presence. It never returns a secret. |
| `credentials_save` | Profile ID and credential object | Saves the endpoint-bound credential in the operating system secure store. |
| `credentials_forget` | Profile ID | Removes the secure credential. The profile remains. |
| `settings_inspect` | None | Returns effective settings and timing guidance. It can initialize or migrate the worker preference. |
| `settings_update` | Complete effective settings object | Checks host settings. It persists the worker preference. Other changes affect this host at the stated boundaries. |
| `serverCollection_inspect` | None | Returns the local server collection intent or null. It makes no server request. |
| `serverCollection_reconcile` | Invocation ID, stop flag, and evidence | Marks a stranded intent as `operatorConfirmedStopped`. It does not stop server work. |
| `capabilities` | None | Discovers installed plugins, categories, settings, targets, and diagnostics. It does not contact Epicor. |
| `connection_test` | Credential source and exact connection scope | Tests login and logout. Uncertain logout retains the connection for explicit disconnect. |
| `connection_connect` | Credential source and exact connection scope | Opens one fixed-scope Epicor connection. |
| `connection_disconnect` | None | Logs out the active connection. Active network runs prevent this action. |
| `companies_list` | None | Returns companies from the active authorized connection. It is read-only. |
| `retrieve_start` | Stable request ID, destination, output mode, and exact identities | Accepts targeted collection where supported. C04 requires paged discovery. C08 exact global library selection ignores the discovery prefix. Poll `run_status`. |
| `backup_start` | Stable request ID, destination, output mode, and optional categories | Discovers within effective plugin options. Omitted values inherit saved options; explicit blank filters clear this run only. Unavailable categories cause rejection. |
| `run_status` | Run `id` and optional `summary` | Returns the saved journal and live progress. It is read-only. |
| `diagnostics_export` | New ZIP path and optional run ID | Writes a sanitized support packet. It preserves an existing target. |
| `run_list` | None | Returns a local run index. It is read-only. |
| `run_pause` | Run `id` | Stops new dispatch for an active run. Current work can finish. |
| `run_resume` | Run `id` | Continues dispatch for a paused active run. |
| `run_cancel` | Run `id` | Requests cancellation and preserves verified files. |
| `run_recover` | Run `id` | Resumes an interrupted network run. Poll `run_status`. |
| `run_retryFailed` | Run `id` | Retries failed or incomplete acquisition. It can contact Epicor again. |
| `run_reprocess` | Run `id` | Reprocesses saved local material. It does not contact Epicor. |

## Connection example

The exact fields depend on `credentialSource` and `authMode`. Read `inputSchema` before creating the request.

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"example-client","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"capabilities","arguments":{}}}
```

## Connect, collect, and inspect results

The following messages continue the initialized session above. Replace the example paths, endpoint, company, and IDs with authorized values.
The profile example uses a user-authored environment file. Its selected entry supplies credentials.
Use the credential-file format in the [user guide](Herbert-Guide.md). Do not place secrets in these examples or saved client logs.

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"connection_connect","arguments":{"credentialSource":{"type":"userFile","path":"C:/Private/environments.json","environment_id":"lab"}}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"backup_start","arguments":{"requestId":"definitions-001","destination":"C:/Backups/Herbert","outputMode":"separate","categories":["C01"],"collectionMethod":"directOnly","temporaryAccess":false}}}
```

A successful connection returns this shape. The values below are illustrative:

```json
{"schemaVersion":1,"ok":true,"result":{"endpoint":"https://example.invalid/instance/","company":"LAB","caller":"operator","authMode":"session","admission":"verifiedSecurityManager","version":"5.2.100.0"},"error":null}
```

Use an available category from `capabilities`. The example explicitly disables temporary access and selects direct collection.
Those choices can reduce available coverage. Inspect the resulting artifact diagnostics.
An accepted backup returns this structured envelope. Generated IDs and paths vary:

```json
{"schemaVersion":1,"ok":true,"result":{"runId":"<returned-run-id>","accepted":true,"destination":"<resolved-output-folder>","scope":"fullBackup"},"error":null}
```

Copy the returned `runId` into each status request:

```json
{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"run_status","arguments":{"id":"<returned-run-id>","summary":true}}}
{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"run_status","arguments":{"id":"<returned-run-id>"}}}
```

Repeat the compact request while `executionActive` is true. After it becomes false, inspect the full response.
The full result includes `phase`, `artifacts`, and `diagnostics`. Artifact evidence determines completeness.
A live host can also return `diagnosticLog` as an object with `path` and `writeFailed`. It can be null when no log exists.
A saved inactive journal can omit that transient field.
The following projection shows the fields used for completion decisions. Other journal fields are omitted for readability.

```json
{"schemaVersion":1,"ok":true,"result":{"runId":"<returned-run-id>","phase":"completed","executionActive":false,"artifacts":[],"diagnostics":[],"diagnosticLog":{"path":"Herbert Data/diagnostics/<log-name>.jsonl","writeFailed":false}},"error":null}
```

Empty artifacts do not prove that requested business content was collected. Compare the result with the requested scope.

After network work and cleanup finish, disconnect in the same session:

```json
{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"connection_disconnect","arguments":{}}}
```

Successful cleanup returns:

```json
{"schemaVersion":1,"ok":true,"result":{"disconnected":true},"error":null}
```

### Failed admission and uncertain logout

A failed admission can also fail logout. The primary error remains in `error.code` and `error.message`.
The cleanup diagnostic appears in `error.context.cleanup`. A true `error.context.connectionRetained` means this host still owns the session.
This is a failed connection attempt. It is not permission to collect data.

1. Preserve the primary and cleanup diagnostics.
2. Call `connection_disconnect` in the same MCP host.
3. If logout fails again, inspect the returned diagnostic before another cleanup attempt.
4. After confirmed cleanup, correct the admission cause before another connection attempt.

A different CLI or MCP process cannot clean up this in-memory session. Keep the original host alive for the retry.
The GUI provides its own `Retry session cleanup` action. Its next Download also retries cleanup before login.

### Saved-run recovery

`run_list` identifies saved runs. Reconnect with the original endpoint, company, and caller before network recovery.
Use the same state and coordination directories that own the run.

```json
{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"run_list","arguments":{}}}
{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"run_recover","arguments":{"id":"<saved-run-id>"}}}
{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"run_status","arguments":{"id":"<saved-run-id>"}}}
```

A successful recovery returns this shape:

```json
{"schemaVersion":1,"ok":true,"result":{"accepted":true,"runId":"<saved-run-id>","operation":"run.recover","destination":"<saved-output-folder>"},"error":null}
```

Poll status until execution stops.
Use `run_retryFailed` for incomplete acquisition. Use `run_reprocess` for local processing of saved material.
Do not automatically repeat a start request with a new request ID after a lost response. First reconcile the existing request with `run_list`.

## Server collection recovery

`concurrency` controls host request scheduling. `serverConcurrency` controls work inside a collector that supports it.
The host records server concurrency in the run journal. Changing it does not make an unsupported helper parallel.
Plugin Function and query-helper invocations share one local collection slot. Direct reads and ordinary file transfers do not acquire that slot.
Hosts with the same coordination directory share an operating-system file lock. This does not coordinate other machines.

An uncertain server response can leave a recorded invocation intent. Later helper collection can fail with `SERVER_COLLECTION_UNRESOLVED`.
Herbert preserves the owned helpers and staged files. Direct work can continue when it does not depend on that invocation.

1. Call `serverCollection_inspect` to obtain the exact `invocationId`.
2. Independently confirm that the server operation finished or stopped.
3. Call `serverCollection_reconcile` with the exact ID and the observed evidence.
4. Resume run recovery to reconcile and remove owned resources.

Client timeout, cancellation, or process exit does not prove that server work stopped.

```json
{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"serverCollection_inspect","arguments":{}}}
{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"serverCollection_reconcile","arguments":{"invocationId":"<recorded-invocation-id>","confirmedServerStopped":true,"evidence":"<independently-observed-server-stop-evidence>"}}}
{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"helpers_reconcile","arguments":{"id":"<recorded-helper-operation-id>"}}}
```

The helper operation ID is distinct from the run ID and invocation ID. Use the ID reported in the helper evidence.
Helper reconciliation requires an inactive operation and its original caller. Ownership and source checks can reject removal.
Server reconciliation records an operator assertion. It does not stop server work or remove server resources.

## Compatibility

Herbert supports Epicor Kinetic 2024.2, 2025.2 and 2026.1. Each plugin reports its own availability and requirements.

The standard package includes the managed decompiler companion and compiled BPM, Function and UBAQ support.
Capture and project creation remain user settings that default off.
A missing helper must not hide useful direct collection.

Check `about`, `capabilities`, and the package documentation after an upgrade. Do not assume that an older client schema still matches the current host.
