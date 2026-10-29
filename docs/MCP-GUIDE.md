# Herbert MCP guide

This guide is for an MCP client that has no prior Herbert context.

Herbert is a local Windows host. It collects authorized Epicor definitions and related files. It can also decompile a local managed assembly.

## Start the server

Run `herbert mcp` as a local stdio server. Send one JSON-RPC message on each input line. Read one response from each output line.

Do not write logs to standard output. Extra output would damage the JSON-RPC stream.

Herbert supports MCP protocol version `2025-11-25`. Start with `initialize`. Then send the `notifications/initialized` notification. Call `tools/list` before tool use.

Use a string or integer request ID. Herbert rejects null, Boolean, object, and fractional IDs. A notification has no `id` member. Herbert does not run tools from notifications.

MCP tool names use an underscore where application operation names use a dot. Letter case does not change. For example, `run.retryFailed` becomes `run_retryFailed`.

Run `herbert mcp-config` from the installed package to print a Claude Desktop `mcpServers` configuration. Herbert uses the actual executable path and resolves the selected local directories to absolute paths. This command does not load profiles, read credentials, create host state, or connect to Epicor. The mode must precede any path flags.

For example, run `herbert mcp-config --state-dir "C:/Herbert State"` to select another state directory. Merge the generated Herbert entry into the client's existing configuration. Preserve unrelated entries.

The package also includes `mcp.claude-desktop.example.json`. It uses fictional paths in the [Claude Desktop configuration format](https://modelcontextprotocol.io/docs/develop/connect-local-servers). A minimal example is:

```json
{
  "mcpServers": {
    "herbert": {
      "command": "C:/Apps/Herbert/herbert.exe",
      "args": ["mcp", "--state-dir", "C:/Apps/Herbert/data", "--plugin-dir", "C:/Apps/Herbert/plugins"]
    }
  }
}
```

Other clients use different configuration keys. See [Connect your AI client](#connect-your-ai-client). Keep credentials out of command arguments, MCP configuration and configuration logs.

Read package-root `AGENTS.md` for release-user operating instructions. Clients do not necessarily load this file automatically. The same instructions are available through `help_read` as the `agent-instructions` document.

The mode must precede the path flags. Each flag requires a directory path:

| Flag | Omitted value | Effect |
| --- | --- | --- |
| `--state-dir` | `data` beside the executable | Stores profiles, preferences, run indexes, and decompiler job records. |
| `--plugin-dir` | `plugins` beside the executable | Selects installed modules and their portable options. |
| `--coordination-dir` | `coordination` under the selected state directory | Stores access ownership and server collection coordination. |

Explicit relative paths resolve from the process working directory. Use absolute paths to avoid client-dependent locations.
Hosts that must coordinate access must use the same coordination directory. This is local coordination, not a distributed server lock.
Plugins ship precompiled, so starting Herbert uses about 1 second of processor time instead of about 2 minutes.
Start one persistent host for a connection workflow. A different process cannot recover an in-memory session from this host.
Closing standard input starts host shutdown. Shutdown cancels active work, waits for cleanup, and attempts logout. Inspect standard error for shutdown diagnostics.

## Connect your AI client

Run `herbert mcp-config --client <client>` from the installed package. It prints that client's configuration with this installation's real paths, and changes nothing. Add `--state-dir`, `--plugin-dir` or `--coordination-dir` to select other folders. Merge the printed entry into the client's existing configuration and keep its other servers.

After you connect, check that the client lists Herbert's 45 tools, such as `capabilities`, `connection_test` and `backup_start`. Then call `about` to confirm the installed version.

### Codex

Codex reads `~/.codex/config.toml` (on Windows, `%USERPROFILE%\.codex\config.toml`). The Codex CLI, IDE extension and desktop app share this file. Run `herbert mcp-config --client codex` and add the printed table. Remove the `#` from a commented line to use that setting:

```toml
[mcp_servers.herbert]
command = 'C:/Apps/Herbert/herbert.exe'
args = ['mcp', '--state-dir', 'C:/Apps/Herbert/data', '--plugin-dir', 'C:/Apps/Herbert/plugins']
# startup_timeout_sec = 30  (Codex default 10)
# tool_timeout_sec = 600  (Codex default 60; raise for long downloads)
# default_tools_approval_mode = "writes"  (prompt only for tools not marked read-only)
```

You can also add it with `codex mcp add herbert -- C:/Apps/Herbert/herbert.exe mcp --state-dir C:/Apps/Herbert/data --plugin-dir C:/Apps/Herbert/plugins`. Run `codex mcp list` to see the entry, and type `/mcp` in Codex to see its tools.

What Herbert needs from Codex:

- Herbert connects to your Epicor server over the network, and reads and writes its state and plugin folders and the backup destination you choose. Codex starts Herbert as a separate process. Codex's documentation does not say that its sandbox settings restrict an MCP server, so don't rely on the Codex sandbox to limit Herbert. Herbert's access is set by its folders, the Epicor account you connect with, and Herbert's own authorization.
- Codex's approval setting decides whether Codex asks you before it calls a Herbert tool. Set `default_tools_approval_mode` in the `[mcp_servers.herbert]` table to `auto`, `prompt`, `writes` or `approve`, or set `approval_mode` for one tool under `[mcp_servers.herbert.tools.<tool>]`. With `writes`, Codex asks only for tools that are not marked read-only. Herbert marks its read-only tools.
- A Codex approval is not a Herbert server-change authorization. Herbert still previews and asks for its own authorization (`serverChanges_preview`, then `serverChanges_authorize`) before any temporary change to the Epicor server. Approving the tool call in Codex does not grant that.

Install the Herbert skill for Codex so Codex knows the workflow. Copy the package's `skills/codex/herbert` folder to `~/.codex/skills/herbert`.

### Claude Code

Run `herbert mcp-config --client claude-code` and save the output as `.mcp.json` in your project folder:

```json
{"mcpServers":{"herbert":{"command":"C:/Apps/Herbert/herbert.exe","args":["mcp","--state-dir","C:/Apps/Herbert/data","--plugin-dir","C:/Apps/Herbert/plugins"]}}}
```

Claude Code asks you to approve a project's `.mcp.json` servers the first time. To add Herbert for all your projects, run `claude mcp add --transport stdio --scope user herbert -- C:/Apps/Herbert/herbert.exe mcp --state-dir C:/Apps/Herbert/data --plugin-dir C:/Apps/Herbert/plugins`. Run `claude mcp list` to check the connection, and `/mcp` in a session to see the tools. The included `skills/herbert` folder is a Claude Code skill: copy it to `.claude/skills/herbert` in your project or to `~/.claude/skills/herbert`.

### Claude Desktop

Run `herbert mcp-config` (or `--client claude-desktop`) and merge the `mcpServers` entry into Claude Desktop's configuration, as shown above. Restart Claude Desktop, then check that Herbert's tools appear.

### Gemini CLI

Run `herbert mcp-config --client gemini` and merge the entry into `~/.gemini/settings.json`, or `.gemini/settings.json` in a project:

```json
{"mcpServers":{"herbert":{"command":"C:/Apps/Herbert/herbert.exe","args":["mcp","--state-dir","C:/Apps/Herbert/data","--plugin-dir","C:/Apps/Herbert/plugins"]}}}
```

Run `/mcp` in Gemini CLI to see the connection and tools, or `gemini mcp list` from the command line. Gemini CLI asks before each tool call unless the server has `"trust": true`. Leave trust off for Herbert.

### VS Code (GitHub Copilot)

Run `herbert mcp-config --client vscode` and save the output as `.vscode/mcp.json` in your workspace. For all workspaces, use **MCP: Open User Configuration** and merge it there. VS Code uses `servers` and requires `"type": "stdio"`:

```json
{"servers":{"herbert":{"type":"stdio","command":"C:/Apps/Herbert/herbert.exe","args":["mcp","--state-dir","C:/Apps/Herbert/data","--plugin-dir","C:/Apps/Herbert/plugins"]}}}
```

Start the server from the file, open Copilot Chat in Agent mode, and check Herbert's tools in the tools picker.

### Cursor

Run `herbert mcp-config --client cursor` and merge the entry into `~/.cursor/mcp.json`, or `.cursor/mcp.json` in a project:

```json
{"mcpServers":{"herbert":{"type":"stdio","command":"C:/Apps/Herbert/herbert.exe","args":["mcp","--state-dir","C:/Apps/Herbert/data","--plugin-dir","C:/Apps/Herbert/plugins"]}}}
```

Check that the server is enabled in Cursor's MCP settings and that its tools are listed. Connection errors appear in the Output panel under MCP Logs.

### Any other client

Herbert is a local stdio MCP server. Configure the client to start the command `C:/Apps/Herbert/herbert.exe` with the arguments `mcp --state-dir <state folder> --plugin-dir <plugins folder>`, using absolute paths. Run `herbert mcp-config --client generic` to print these with your real paths. No environment variables, network port or URL are needed. Use forward slashes, or escape backslashes in JSON and TOML.

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

On failure, `ok` is false and `isError` is true. `result` is null. `error` is a diagnostic with these fields:

- `code`: a stable error code. Branch on the code, not on the message.
- `message`: plain text that says what happened and, where Herbert knows it, what to do next.
- `retryable`: whether the same request can succeed later without a change.
- `context`: the technical details, such as paths, IDs, lower-level errors and server text. The app shows the same details under a collapsed **Technical details** section. For example, a refused BAQ import includes Epicor's import log here. It is omitted when empty.
- `identity`: the affected item, when there is one.

Use `structuredContent` as the authoritative result. `content[0]` is the same JSON for clients that only read text. On failure, `content[1]` is plain text: the message, then a `Next step:` line when the message does not already give one. When that step asks for diagnostics, the text also names the `diagnostics_export` tool.

Error codes and field names are stable. Messages can be reworded between versions.

## Safe workflow

1. Call `help_read` and read the `agent-instructions` document.
2. Call `capabilities` to discover installed categories, their IDs and their availability.
3. Read `settings_inspect` and `collection_preferences_get`.
4. Configure plugin options only when the discovered schema requires them.
5. Create or select a profile. Save credentials only when local storage is acceptable.
6. Call `connection_test` to test the exact endpoint, company, caller, and authentication mode. Without a company it lists the companies the account can use (`availableCompanies`), then signs out. See [Company discovery](#company-discovery).
7. Call `connection_connect` with a company before an Epicor collection.
8. Call `serverChanges_preview` with `fullBackup` and the effective `run` fields, including a stable `requestId`. See [Server-change preview and authorization](#server-change-preview-and-authorization).
9. Read the material effects, `unresolved` facts and `matchingAuthorizations`. Reuse a matching authorization without asking again. If `unresolved` is not empty, do not authorize. Narrow the request, or preview again with `collectionMethod:"directOnly"` and `temporaryAccess:false`.
10. If `authorizationRequired` is true and nothing matches: when the current human instruction already covers that target, material scope and lifetime, create the authorization. Otherwise obtain explicit authority before `serverChanges_authorize`. An ordinary backup request does not authorize server changes.
11. Call `backup_start` or `retrieve_start` with the same run fields and `authorizationId` set to the authorization's `id`. Keep helper permission and temporary-access permission explicit and independent.
12. Poll `run_status` until `executionActive` is false.
13. Inspect completeness, diagnostics, artifacts and `serverChanges` separately. Keep useful partial output and report missing content or pending cleanup.
14. Call `connection_disconnect` after all network runs stop.

A request with `collectionMethod:"directOnly"` and `temporaryAccess:false` usually makes no server change. Its preview then returns `authorizationRequired:false`, and the run needs no `authorizationId`. Some native exports still create temporary server files and need authorization.

Do not treat an accepted run as a completed run. Do not treat a completed phase as final while `executionActive` is true.

## Defaults and operating choices

A fresh host uses these initial values before saved preferences or explicit settings apply:

- Collection method is `directOnly`.
- Host concurrency is 4.
- Server concurrency is 4.
- Local decompiler workers are 4.
- Native page size is 500.
- Temporary access is off.
- Output mode must be `refresh` or `separate`.
- Retry count is 0 unless settings specify another value.
- Connection, service, export, and inactivity timeout values start as null.
- `minimumRequestIntervalMs` and `retryBackoffMs` start as null.

These values are operating defaults. They are not general workload limits. Read `settings_inspect` for the effective values and timing guidance.

The host page size accepts a whole number from 1 to 2,147,483,647. Blank (null) uses the default. Any other value is refused with `INVALID_SETTING` and a message about the page size alone.

Omitted API run permissions always mean `directOnly` and temporary access off. Saved GUI preferences can restore explicit GUI choices. Preferences do not create server-change authority. A saved workflow must supply its explicit run fields and workflow identity.
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

A `userFile` source reads a user-authored JSON file. Select the entry with `environmentId`. The older `environment_id` spelling is still accepted. Herbert does not edit that file. Keep the file outside source control and download folders.

Profiles and host preferences use the selected state directory. Plugin options live beside each module in its portable state file.
Run output folders contain collected files and recovery journals. Coordination records use the selected coordination directory.

## Side effects and native changes

Tool annotations are hints. Read the stated effects before use.

On first use, `settings_inspect` can create `decompiler-preferences.json`. It can migrate the legacy Functions worker choice into that file.
After Herbert saves the host preference, migration removes only the legacy worker field from plugin state. Existing host preferences take precedence.
Other initialization paths include `settings_update`, `plugin_configure`, `retrieve_start`, `decompiler_start`, and `decompiler_recover`.

When the run explicitly permits and authorizes it, `backup_start` and `retrieve_start` can use temporary native access, and server-assisted plugins can create temporary native helpers. Herbert records ownership and cleanup evidence.

`helpers_reconcile` can remove owned temporary native resources. Use the exact helper operation ID. The original caller and source checks must pass.

`serverCollection_reconcile` changes local coordination state after you supply independent server stop evidence. It does not stop a server process.

Unsaved server queries that Herbert runs are named `Herbert_…`. Their company parameters are named `HerbertCompany_…`. Collected JSON files carry Herbert's own metadata under `_herbertAcquisition` and `_herbertRetention`.

## Category IDs

Each category has a stable ID, such as `ud-codes`, `ud-schema`, `extended-properties`, `baqs-and-ubaqs`, `functions` or `searches-and-navigation`. Read the installed IDs and display names from `capabilities`. Use the ID in `categories`, `identities[].category`, the keys of `pluginSettings` and the `category` argument of `plugin_configure`.

- A retired category code from an earlier version is refused with `LEGACY_CATEGORY_ID`. `error.context.retiredCategory` holds the code you sent and `error.context.replacementCategory` names the current ID. Send the request again with that ID.
- Runs, settings and plugin state saved by earlier versions are still read. Status and support packets show them with the current IDs and names.
- Refreshing a backup made by an earlier version rewrites its `Herbert Data` metadata with the current IDs. This is one-way: the earlier version cannot refresh that backup afterwards.
- A plugin whose own ID is a retired category code is refused at load with `PLUGIN_ID_RETIRED`. It appears in the `capabilities` diagnostics. Ask the plugin author for a different ID.

## Company discovery

Call `connection_test` without `company` to list the companies the account can use. Herbert always signs out afterwards. The result adds:

| Field | Meaning |
| --- | --- |
| `availableCompanies` | `[{id, name}]`, sorted by `id`. |
| `companySource` | `userAccess`: the account's own company access. `companyList`: all companies, and access is confirmed when you connect. `unavailable`: the list could not be read. |
| `selectedCompany` | `{id, inList}` when you supplied a company, otherwise null. `inList` says whether that company is in `availableCompanies`. |
| `companyListDiagnostic` | Why the list could not be read, or null. |

`company` is null in that result. `capabilities.connection.companyDiscovery` is true when the host supports this.

A collection needs a company. `connection_connect` without one is refused with `COMPANY_REQUIRED`, and so is a download over a connection that only listed companies. The app disables Download until a company is chosen. `profiles_upsert` still saves a profile without a company, and returns the warning `COMPANY_NOT_SET` in `warnings[]`.

If Herbert cannot confirm the sign-out after a test, it keeps that session only so that `connection_disconnect` can retry the sign-out. No collection uses it. `error.context.connectionRetained` is true. Call `connection_disconnect` in the same host, then connect again. Until then, collection requests return `CONNECTION_REQUIRED` with `error.context.recoveryOperation` set to `connection.disconnect`.

## Server-change preview and authorization

Herbert changes the Epicor server only through declared effects: installing or invoking a helper, adding an eligible group membership (temporary access), or creating a temporary native export file. Each run that can cause one of these needs an authorization that covers it. Account eligibility, saved preferences and ordinary download requests never authorize a change.

New downloads default to `collectionMethod:"directOnly"` with `temporaryAccess:false` when those fields are omitted.

`serverChanges_preview` takes `fullBackup` and `run`. Use `fullBackup:true` with `backup_start` arguments, or `false` with `retrieve_start` arguments. Include a non-empty `requestId`. Preview reads only. It does not install or invoke helpers, borrow access, export files or clean up. It needs an active connection.

| Preview field | Meaning |
| --- | --- |
| `previewId` | Pass to `serverChanges_authorize`. Valid in this host's current connection session. |
| `materialDigest` | Fingerprint of the material scope. Pass it unchanged. |
| `materialScope` | Target, account, plugins, helper packages and effects that the authorization would bind. |
| `display` | The readable plan: `accountReadiness`, `endpoint`, `company`, `principal`, `destination`, `collectionMethod`, `temporaryAccess`, `possibleServerChanges`, `priorCleanup`, `trust`, and per plugin in `capabilities[]` its `effectiveSettings`, `helpers`, `helperPermission` and `nativeFileEffects`. |
| `unresolved` | Facts Herbert could not establish. Authorization is refused while this is not empty. |
| `authorizationRequired` | False when the request makes no server change. |
| `readOnly` | Always true. |
| `matchingAuthorizations` | Saved authorizations that already cover this request. Reuse one instead of asking again. |

`display.accountReadiness.state` is `verifiedEligible` with `admission:"verifiedSecurityManager"` when the connection passed the Security Manager admission checks. Eligibility is not authorization.

`serverChanges_authorize` takes `previewId`, `materialDigest`, `lifetime` and an optional `expiresAt` (a Unix time in seconds that the user chose). It returns `authorization`, the saved grant, and `reconciled`. Pass `authorization.id` as `authorizationId` to `backup_start` or `retrieve_start`. Repeating the same acknowledgement returns the same grant with `reconciled:true`. A different acknowledgement for the same preview is refused.

| `lifetime` | Applies to |
| --- | --- |
| `{"kind":"oneRun"}` | One logical request (its `requestId`), including its ordinary recovery. |
| `{"kind":"session"}` | Runs in this connection session. It ends when a new connection opens. |
| `{"kind":"workflow","workflowId":"<name>"}` | Runs that supply the same `workflowId`. It never schedules anything. |

A narrower selection or harmless tuning reuses an authorization. A different target, account, helper package or added effect needs a new preview. A stale or changed preview is refused with `SERVER_CHANGE_PREVIEW_REQUIRED`. A run start without a covering authorization is refused with `SERVER_CHANGE_PREVIEW_REQUIRED` or `SERVER_CHANGE_AUTHORIZATION_REQUIRED`. Preview again and review the changes.

`serverChanges_list` returns `authorizations`. `serverChanges_inspect` and `serverChanges_revoke` take the authorization `id`. Revocation records `revokedAt` and stops new optional changes at their next check. Cleanup of resources Herbert already owns continues under the original authorization. Revocation deletes nothing on the server. The app lists saved authorizations with a **Revoke** action.

```json
{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"serverChanges_preview","arguments":{"fullBackup":true,"run":{"requestId":"definitions-002","destination":"C:/Backups/Herbert","outputMode":"separate","categories":["extended-properties"],"collectionMethod":"serverAssisted","temporaryAccess":false}}}}
{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"serverChanges_authorize","arguments":{"previewId":"<previewId>","materialDigest":"<materialDigest>","lifetime":{"kind":"oneRun"}}}}
{"jsonrpc":"2.0","id":22,"method":"tools/call","params":{"name":"backup_start","arguments":{"requestId":"definitions-002","destination":"C:/Backups/Herbert","outputMode":"separate","categories":["extended-properties"],"collectionMethod":"serverAssisted","temporaryAccess":false,"authorizationId":"<authorization.id>"}}}
```

## Server changes in run status

`run_status` reports the authorization a run used as `serverChangeAuthority`, or null for a run that may change nothing. It reports the certificate trust recorded at run start as `transportTrust`.

`run_status` also returns `serverChanges`, separate from collection completeness:

- `leaseLost` is true when the run lost its server lease for temporary helpers (`SERVER_LEASE_LOST`). New helper work stops and affected items are incomplete. Direct reads and cleanup continue. A helper call running at that moment is reported as `SERVER_LEASE_LOST_DURING_CALL`, and its result is not used.
- `resources[]` lists server changes whose cleanup is not finished, each with `code`, `identity`, `context` and `outcome` (for example `pending` or `uncertain`).
- `foreignPending[]` lists helpers that this run found and kept. Each `state` is `active` (a recognized Herbert helper under another run's live lease, removed by a later cleanup once that lease ends), `recognized` (found when this run had no lease host) or `unrecognized` (looks like a Herbert helper but couldn't be confirmed; reported with `CLEANUP_AUTHORITY_REQUIRED`). Helpers left by an earlier Herbert version that match a released Herbert helper and that no running Herbert holds are removed automatically and do not appear here.

Cleanup removes only resources the run owns. Report pending, uncertain and foreign items with their identities.

## Discovery filters, exact retrieval and persistence

Read the installed category settings and help from `capabilities` before constructing `pluginSettings`. The plugin defines the match rule. A selection setting's help states its rule and ends with `Matching contract: <id>`. That ID is recorded with each run in `pluginSelectionContracts`. `backup_start` discovers roots within those effective options; `retrieve_start` selects exact native identities independently of discovery filters where the plugin supports acquisition. Processing options still apply. BAQs and UBAQs currently supports paged discovery only; use its Query ID prefix with `backup_start`.

Omitted run values inherit the plugin's saved values. A fresh plugin with no saved filter discovers all currently eligible roots within its existing company, tenant, ownership, system and access policies. An explicit blank filter override clears that filter for one run. Run overrides do not save options. `plugin_configure` saves declared values; saving a blank clears the saved filter. In the GUI, category toggles, Select all available, Clear selection and Save plugin options also save the current option draft. Opening the GUI does not save it.

The UD codes plugin declares `codeTypeIdPrefix`: **Code type ID starts with**. It is a literal, case-sensitive prefix. Every entered character, including spaces, is preserved; there is no trimming, case folding, wildcard expansion or Unicode normalization. The matching contract is `ud-codes-code-type-id-prefix-v1`. Herbert reads native GetRows pages in bulk with the broad server match described below, then selects complete code types with all eligible entries locally. Native paging continues through empty selected pages.

Most ID-prefix filters work the same way. Herbert sends the server a broad match on the prefix's leading characters up to its first wildcard character (`%`, `_`, `[` or `]`), for example `LIKE 'EXAMPLE%'` (an OData `startswith` filter for Access and assignments), and applies the exact literal, case-sensitive prefix locally; rows outside the selection are not validated and produce no diagnostics. Exceptions: Menus and launch definitions (a selected menu's parents need not match the prefix), Searches and navigation, Business rules and mappings, the Report styles and layouts `packagePathPrefix` and the Programs and extensions externals list read every eligible row and select locally. Kinetic applications and layers reads a light identity-only inventory, selects locally and downloads content only for selected applications. Sources with no server filter (Quick, Predictive and context-menu searches, and current stored report packages) are read whole, and rows outside the selection still produce no diagnostics. A family or report type left unselected is not requested at all. Automation and integrations asks Epicor only for the rows its native service admits for the caller, restricting each table's read to those row identities, so rows outside the caller's visibility are never downloaded.

For an authorized UD codes discovery, add `"pluginSettings":{"ud-codes":{"codeTypeIdPrefix":"EXAMPLE"}}` to the start arguments below. To clear a saved prefix for that run, send `"pluginSettings":{"ud-codes":{"codeTypeIdPrefix":""}}`. Exact `UDCodeType` retrieval uses its native `CodeTypeID` identity and ignores this discovery prefix. Do not invent another ID-list syntax.

The BAQs and UBAQs plugin and the Functions plugin use the same literal prefix rule and blank/omission persistence behavior:

| Category | Setting / label | Matching contract | Complete selected unit |
| --- | --- | --- | --- |
| BAQs and UBAQs | `queryIdPrefix` / Query ID starts with | `baqs-and-ubaqs-query-id-prefix-v1` | Whole BAQ or UBAQ, diagrams, required dependency metadata and authored update methods/workflows |
| Functions | `libraryIdPrefix` / Library ID starts with | `functions-library-id-prefix-v1` | Whole global library, all Functions, signatures, references and company mappings |

For BAQs and UBAQs discovery, use `"pluginSettings":{"baqs-and-ubaqs":{"queryIdPrefix":"EXAMPLE"}}`. Exact acquisition through `retrieve_start` is not supported for BAQs and UBAQs. For Functions discovery, use `"pluginSettings":{"functions":{"libraryIdPrefix":"EXAMPLE"}}`. Exact Functions `EfxLibrary` retrieval uses global scope and `LibraryID`, independently of the discovery prefix. Company mappings remain library content. The Functions plugin passes only selected inventory IDs to its existing bulk GetLibraries read. Both collectors retain native continuation through empty selected pages. Required dependency/reference rows can name IDs outside a prefix; they remain with the selected artifact. Referenced libraries are not automatically collected.

Optional compiled capture and projects remain separate settings, default off, and apply to selected owners. Normal native packages save independently before optional capture. Direct only retains normal output and reports any requested helper-dependent gap.

The journal's `scope:fullBackup` is the persisted discovery execution mode. It does not assert full-environment coverage. Inspect frozen `pluginSettings`, `pluginSelectionContracts` (original plugin-declared filter descriptions and matching version), `pluginHashes`, discovered identities, category outcomes and artifact completeness. Compact status also carries those frozen options, metadata and fingerprints. The GUI shows them under **Run scope and frozen options**. Do not infer inactive filters by comparing values with descriptor defaults: saved values become those defaults.

A completed discovery with `discoveryFinished[category]:true` and `categoryOutcomes[category]:verifiedAbsent` is a successful empty selection: no eligible items matched the run selection. `run_status.discoveryMessages` presents that message only after committed overall discovery is complete and its total selected roots are empty. It does not establish that the environment has no artifacts outside that selection. An empty intermediate page is not completion. Invalid setting types, missing exact identities and failed/denied reads remain separate failures; an invalid filter never broadens into unfiltered collection.

Recovery and retry use frozen values. A changed selection starts a new logical request. Existing plugin-hash checks still apply. If an older journal omits a declared filter, this host refuses guest dispatch with `FROZEN_SELECTION_REQUIRED` rather than applying today's saved filter. Restore the original compatible module or start a new run with an explicit selection. Saved output remains preserved. Refresh preserves unrelated and user-edited files; an item outside the selected scope is not a deletion request.

For a read-only check, select one plugin and the smallest useful existing selection. Omitted run permissions already mean Direct only and temporary access off. Send `"collectionMethod":"directOnly","temporaryAccess":false` explicitly anyway, so the request describes itself. Saved preferences never establish server-change authority.

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
| `serverChanges_preview` | `fullBackup` and `run` (start arguments) | Plans possible server changes without changing Epicor. Returns `previewId`, `materialDigest`, `materialScope`, `display`, `unresolved`, `authorizationRequired`, `readOnly` and `matchingAuthorizations`. It is read-only and needs an active connection. |
| `serverChanges_authorize` | `previewId`, `materialDigest`, `lifetime`, optional `expiresAt` | Saves an explicit authorization for that preview. Returns `authorization` and `reconciled`. Creates no schedule and starts nothing. |
| `serverChanges_list` | None | Returns saved `authorizations`. It is read-only. |
| `serverChanges_inspect` | Authorization `id` | Returns one saved authorization. It is read-only. |
| `serverChanges_revoke` | Authorization `id` | Records `revokedAt` and stops new optional changes. Owned cleanup continues. Deletes nothing on the server. |
| `decompiler_start` | Assembly path, new output folder, and optional portable symbols | Accepts a local job. It snapshots input and writes a project tree. Poll `decompiler_status`. |
| `decompiler_status` | `id` | Returns saved job state, files, hashes, diagnostics, and live statistics. It is read-only. |
| `decompiler_cancel` | `id` | Requests cancellation. Poll `decompiler_status` for the final state. |
| `decompiler_recover` | `id` | Resumes from verified snapshots. It requires exclusive ownership of the output folder. |
| `collection_preferences_get` | None | Returns the saved default collection method. It is read-only. |
| `collection_preferences_set` | `collectionMethod` | Saves `directOnly` or `serverAssisted` for future runs. |
| `schema` | None | Returns every application operation and its schemas. It is read-only. |
| `help_read` | None | Returns the embedded user, technical and MCP guides and the `agent-instructions` document. It is read-only. |
| `about` | None | Returns product identity, version, credits, and public links. It is read-only. |
| `helpers_reconcile` | Exact helper operation `id` | Reconciles owned native resources. This can change the authorized server. |
| `plugin_configure` | Category, optional selection, and declared setting values | Saves portable plugin state. It does not contact Epicor. |
| `profiles_list` | None | Returns local profiles without credential values. It is read-only. |
| `profiles_upsert` | Profile `id` and `url`; optional name, authentication mode, defaults, file reference and `allowUntrustedCertificate` | Creates or replaces one local profile. Returns `saved` and `warnings`, such as `COMPANY_NOT_SET`. Credentials remain separate. |
| `profiles_rename` | Profile ID and name | Changes only the display name. |
| `profiles_remove` | Profile ID | Removes the profile. Remove its secure credential separately. |
| `profiles_associateFile` | Profile `id`, `path`, and exact `environmentId` | Saves a file reference. The user file remains unchanged. |
| `profiles_removeFile` | Profile ID | Removes the saved file reference. The user file remains unchanged. |
| `credentials_status` | Profile ID | Reports secure credential presence. It never returns a secret. |
| `credentials_save` | Profile ID and credential object | Saves the endpoint-bound credential in the operating system secure store. |
| `credentials_forget` | Profile ID | Removes the secure credential. The profile remains. |
| `settings_inspect` | None | Returns effective settings and timing guidance. It can initialize or migrate the worker preference. |
| `settings_update` | Complete effective settings object | Checks host settings. It persists the worker preference. Other changes affect this host at the stated boundaries. |
| `serverCollection_inspect` | None | Returns the local server collection intent or null. It makes no server request. |
| `serverCollection_reconcile` | Invocation ID, stop flag, and evidence | Marks a stranded intent as `operatorConfirmedStopped`. It does not stop server work. |
| `capabilities` | None | Discovers installed plugins, category IDs, settings, targets, `temporaryAccess` guidance, `connection` support (`allowUntrustedCertificate`, `companyDiscovery`) and diagnostics. It does not contact Epicor. |
| `connection_test` | Credential source and connection scope; company and `allowUntrustedCertificate` optional | Tests login and logout. Returns `availableCompanies`, `companySource`, `selectedCompany`, `companyListDiagnostic` and `transportTrust`. Uncertain logout retains the connection for explicit disconnect. |
| `connection_connect` | Credential source and exact connection scope; optional `allowUntrustedCertificate` | Opens one fixed-scope Epicor connection and returns `transportTrust`. A company is required (`COMPANY_REQUIRED`). |
| `connection_disconnect` | None | Logs out the active connection. Active network runs prevent this action. |
| `companies_list` | None | Returns companies from the active authorized connection. It is read-only. |
| `retrieve_start` | Stable request ID, destination, output mode, exact identities, and `authorizationId` when the preview requires one | Accepts targeted collection where supported. Omitted permissions mean Direct only with temporary access off. The BAQs and UBAQs plugin requires paged discovery. For Functions, exact global library selection ignores the discovery prefix. Poll `run_status`. |
| `backup_start` | Stable request ID, destination, output mode, optional categories, and `authorizationId` when the preview requires one | Discovers within effective plugin options. Omitted permissions mean Direct only with temporary access off. Omitted values inherit saved options; explicit blank filters clear this run only. Unavailable categories cause rejection. |
| `run_status` | Run `id` and optional `summary` | Returns the saved journal and live progress, including `serverChangeAuthority`, `transportTrust` and `serverChanges`. It is read-only. |
| `diagnostics_export` | New ZIP path and optional run ID | Writes a sanitized support packet. It preserves an existing target. |
| `run_list` | None | Returns a local run index. It is read-only. |
| `run_pause` | Run `id` | Stops new dispatch for an active run. Current work can finish. |
| `run_resume` | Run `id` | Continues dispatch for a paused active run. |
| `run_cancel` | Run `id` | Requests cancellation and preserves verified files. |
| `run_recover` | Run `id` | Resumes an interrupted network run. Poll `run_status`. |
| `run_retryFailed` | Run `id` | Retries failed or incomplete acquisition. It can contact Epicor again. |
| `run_reprocess` | Run `id` | Reprocesses saved local material. It does not contact Epicor. |

## Allow self-signed certificate

The app labels this option **Allow self-signed certificate**. `allowUntrustedCertificate` (profile field, environment-file field and `connection_test`/`connection_connect` argument, default false) accepts a server certificate that Windows does not trust because it is self-signed or from a private certificate authority. Herbert still checks the server name, the validity dates and the handshake signature. Only the unknown-issuer check is relaxed. It applies to that profile's endpoint only. Turn it on only when the user explicitly asks; never infer it.

Know its limits:

- Any certificate the server presents can act as the trust anchor, so an expired intermediate certificate is not refused.
- Trust is recorded at run start. Nothing is pinned, so a reconnect in the same run that presents a different certificate is accepted without a warning.
- Use the option only for servers you control.

`connection_test`, `connection_connect` and `run_status` report `transportTrust`:

| Field | Meaning |
| --- | --- |
| `mode` | `verified`: Windows trusted the certificate. `relaxedUntrustedIssuer`: the issuer was not trusted and the profile allows that. The server name and dates still passed. `unencrypted`: plain `http://`, so no certificate was presented. |
| `certificateSha256` | Fingerprint of the server certificate. It can change whenever the server renews its certificate. |
| `subject`, `issuer`, `notAfter` | Public certificate facts, for display. |

The certificate fields are omitted when `mode` is `unencrypted`. For that mode the app warns that the connection is not encrypted, and the support packet says "Not encrypted: plain http, no certificate". `capabilities.connection.allowUntrustedCertificate` is true when the host supports the option.

Certificate failures use `CERTIFICATE_UNTRUSTED`, `CERTIFICATE_NAME_MISMATCH`, `CERTIFICATE_EXPIRED`, `CERTIFICATE_NOT_YET_VALID` or `CERTIFICATE_INVALID`. They are not retried. `error.context` contains `host` and `allowUntrustedCertificate`. Only `CERTIFICATE_UNTRUSTED` can be resolved with this option, and only when the user trusts the server.

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
The profile example uses a user-authored environment file. Its selected entry supplies the endpoint and credentials, and its `defaults.company` supplies the company. If the entry has no company, add `"company"` to the arguments, or the connection is refused with `COMPANY_REQUIRED`.
Use the credential-file format in the [user guide](Herbert-Guide.md). Do not place secrets in these examples or saved client logs.

```json
{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"connection_connect","arguments":{"credentialSource":{"type":"userFile","path":"C:/Private/environments.json","environmentId":"example"}}}}
{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"backup_start","arguments":{"requestId":"definitions-001","destination":"C:/Backups/Herbert","outputMode":"separate","categories":["ud-codes"],"collectionMethod":"directOnly","temporaryAccess":false}}}
```

A successful connection returns this shape. The values below are illustrative:

```json
{"schemaVersion":1,"ok":true,"result":{"endpoint":"https://example.invalid/instance/","company":"EXAMPLE","caller":"operator","authMode":"session","admission":"verifiedSecurityManager","version":"5.2.100.0","transportTrust":{"mode":"verified","certificateSha256":"<sha256>"}},"error":null}
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

## Report style restore notes

A report style with more than one style rule is saved with `completeness` `partial` and the diagnostic `REPORT_STYLE_MULTIPLE_RULES`, whose `context.ruleCount` gives the number of rules. Epicor imports at most one rule per style. Keep one rule in the saved XML before importing.
When a style has a CompanyList, its output includes a `<style>-RESTORE.txt` note with the saved value, because Epicor's importer ignores CompanyList. Paths inside the generated `reports.zip` are relative to the server's reports folder.

## Compatibility

Herbert supports Epicor Kinetic 2024.2, 2025.2 and 2026.1. Each plugin reports its own availability and requirements.

The standard package includes the managed decompiler companion and compiled BPM, Function and UBAQ support.
Capture and project creation remain user settings that default off.
A missing helper must not hide useful direct collection.

Check `about`, `capabilities`, and the package documentation after an upgrade. Do not assume that an older client schema still matches the current host.
