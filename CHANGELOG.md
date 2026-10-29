# Changelog

All notable changes to Herbert are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-10-28

### Added

- Support for Epicor Kinetic 2024.2.
- A per-profile "Allow self-signed certificate" option (MCP: `allowUntrustedCertificate`) for servers whose certificate Windows does not trust. Connection results, download status and support packets show which trust mode was used (`transportTrust`).
- Test connection lists the companies your account can use, then signs out. Over MCP, `connection_test` without a company returns `availableCompanies`, `companySource` and `selectedCompany`.
- `capabilities.connection` reports `allowUntrustedCertificate` and `companyDiscovery`.
- Error replies to MCP clients include a second plain-text item with the message and a next step. When the step is to save diagnostics, it names the `diagnostics_export` tool. The structured reply is unchanged.
- `run_status` reports `serverChanges`: whether the server lease was lost (`leaseLost`), helper resources whose cleanup is pending or uncertain (`resources`), and helpers left alone because they belong to another run (`foreignPending`).
- "Saved authorizations…" on Download lists your saved session and named-workflow authorizations, shows what each allows and when it was created, and lets you revoke one after confirming.
- `herbert mcp-config --client` prints a ready-to-use configuration for Codex, Claude Code, Claude Desktop, Gemini CLI, VS Code (GitHub Copilot), Cursor or any other stdio MCP client. The MCP guide explains where each client keeps it and how to check that Herbert's tools are listed.
- A Herbert skill for Codex, in `skills/codex/herbert`, alongside the Claude Code skill.
- This changelog.

### Changed

- Collections now have stable IDs such as `ud-codes` and `baqs-and-ubaqs`. Use them in MCP requests and plugin settings.
- MCP requests that use a retired category code are refused with `LEGACY_CATEGORY_ID`, which names the replacement ID.
- Runs, settings and installed plugins saved by earlier versions are still read, and are shown by name.
- Refreshing a backup that an earlier version created updates its Herbert Data metadata to the new collection IDs. After that, earlier versions can't refresh that backup, so keep using 0.2.0 or later for it. Backups that 0.2.0 has not refreshed are unchanged.
- A download cannot start without a company. The app disables Download, and MCP returns `COMPANY_REQUIRED`. Saving a profile without a company returns the warning `COMPANY_NOT_SET`.
- Certificate failures return specific codes: `CERTIFICATE_UNTRUSTED`, `CERTIFICATE_NAME_MISMATCH`, `CERTIFICATE_EXPIRED`, `CERTIFICATE_NOT_YET_VALID` and `CERTIFICATE_INVALID`.
- Plain http connections report the trust mode `unencrypted` instead of `verified`. The app and support packets say the connection is not encrypted.
- The user-file credential source uses `environmentId`. The older `environment_id` is still accepted.
- Unsaved queries that Herbert runs on the server are named `Herbert_…` and `HerbertCompany_…`.
- Metadata keys in backup output are renamed to `_herbertAcquisition` and `_herbertRetention`. Backups written with the earlier keys are still read.
- Plugin setting values are never mistaken for retired category codes. A plugin whose ID is a retired code is refused when it loads (`PLUGIN_ID_RETIRED`).
- Error and status messages are written for the person reading them: each one says what happened and what to do next. Details such as file paths, IDs, lower-level errors and text returned by the server are now in the diagnostic's `context` (shown under Technical details in the app, and returned to MCP clients), not in the message. Error codes and field names are unchanged.
- Page size has its own error message and accepts values up to 2,147,483,647, the same limit every plugin uses.
- Error notices, including a failed attempt to open the download folder, have a collapsed Technical details section with the cause, the operating system's error code and Epicor's message.
- When Epicor refuses a BAQ import, its import log is included in the error details.
- Error notices and run issues end with a next step when the message doesn't already give one.
- If Herbert loses its server lease, it stops starting new helper work and marks the affected items incomplete. Direct reads and cleanup of its own helpers continue, and the download is not canceled (`SERVER_LEASE_LOST`). A helper call running at that moment is reported as uncertain and is not retried (`SERVER_LEASE_LOST_DURING_CALL`).
- At the end of a download, Herbert removes only helpers that the download itself created. Helpers left on the server by an earlier Herbert version are removed automatically when Herbert recognizes them as its own released helpers and no running Herbert is using them. Anything that looks like a Herbert helper but can't be confirmed is kept and reported (`CLEANUP_AUTHORITY_REQUIRED`).
- Report styles with more than one style rule are still saved, but are marked incomplete with `REPORT_STYLE_MULTIPLE_RULES`, because Epicor's import refuses them. The message explains how to import the style.
- When a report style uses CompanyList, Herbert writes a `<style>-RESTORE.txt` note beside the style XML with the value to set again after importing, because Epicor's import ignores it.
- Plugin cards show the name, description and all options. The longer help is under a collapsed "About this plugin" section.
- "New download" and "Authorize these changes and start" use the same highlighted button style as "Save" and "Preview download".
- The run heading says "Starting…", "Preparing…", "Recovering…", "Reprocessing…" or "Working…" instead of an internal phase name.
- Hovering over the run progress bar shows about how much of the known work is done.
- Inactive tabs highlight when you hover over them.
- When building from source, the compiler cache is kept bounded automatically, and `scripts/prune_build_cache.py --wipe` clears it on demand.
- ID-prefix filters are sent to the server as a broad match wherever the source supports a filter, so less data is downloaded. The exact prefix is still checked locally, and rows outside your selection are no longer checked or reported.
- Collections skip server requests for families you didn't select.
- Several collections make far fewer server requests, and long downloads spend less time saving their progress.
- Herbert releases memory after each download finishes, so repeated downloads no longer keep growing its memory use.
- A saved search discovery that had already passed its first page must be restarted. Herbert refuses to resume it with "Restart search discovery".
- Custom resources reads translations in pages, using the host's page size setting.
- A new installation or profile starts with 17 collections selected. Classic customizations, Classic dashboards, Classic personalizations, Configurators, Kinetic personalizations, Programs and extensions, and Solutions and packages start off. Saved choices are kept.
- Help text and plugin descriptions describe only what you need to use Herbert, and no longer point to documents that are not included.
- Comments in the server helpers that Herbert installs no longer contain internal notes.
- Labels use sentence case. MCP values are unchanged.
- License updated to version 1.1.

### Removed

- The `temporaryAccess.qualified` field from `capabilities`. It was always false and carried no information.

### Fixed

- Starting Herbert uses about 1 second of processor time instead of about 2 minutes, because plugins now ship precompiled.
- The decompiler companion no longer contains a path from the machine that built it.
- Kinetic personalizations no longer sends a server request for an operation it doesn't support, such as inspect. This affected 0.1.1.
- The progress view shows the current stage at the default window size again. "What this download includes" (formerly "Run scope and frozen options") moved to a menu on the "Saving to:" row.
- A long destination path in the progress view is shortened, with a tooltip and a Copy button for the full path.
- An error opening the server lease file now gives the correct advice.
- The About tab and the run animation use much less processor time. Animations run at a steady frame rate, slow down when the window is in the background, and stop while it is minimized. They look the same.
- Animations no longer stutter after Herbert has been open for many hours.
- The Download header no longer ends with a stray "·" when no company is set.
- Download-folder errors point to the path after "Saving to:", and say what to do when File Explorer can't be found or the folder hasn't been created yet.
- reports.zip stores each report relative to the server's reports folder, so importing it no longer nests reports under an extra `reports/` folder.
- Herbert treats its server lease as lost 15 seconds before the server would let it expire, and retries a failed renewal while time remains. A helper call can no longer start on a lease the server has already dropped.
- A read that relied on temporary access released partway through is reported as incomplete and can be retried (`ACCESS_MEMBERSHIP_LOST_DURING_READ`), instead of being saved as complete.
- If Herbert can't confirm the sign-out after listing companies, it uses that connection only to connect, test or disconnect. Disconnecting retries the sign-out.

### Security

- Allowing a self-signed certificate relaxes only the unknown-issuer check, so a private certificate authority is accepted. Herbert still checks the server name and the server certificate's dates.
- Revoking a server-change authorization stops new helper calls immediately. Calls already running can finish, and Herbert still cleans up the helpers it created.
- Access borrowed from another account is used only for the same server, company and requester. A mismatch is refused before anything is read (`ACCESS_SCOPE_MISMATCH`).

## [0.1.1] - 2026-10-27

### Added

- Herbert backs up your Epicor customizations to a local folder, from the app, the command line or MCP.
- Signed Windows executables.
