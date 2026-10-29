# Sanitized support packets

Choose **Save diagnostics…** in the download progress panel. It works during and after a run without pausing it. A failed setup also offers the button before a run exists. Save to a new ZIP filename; Herbert preserves existing files. The regular issue review and **Copy private details** remain local and can contain product names. Share the sanitized ZIP instead.

The ZIP contains summary.json, events.jsonl and README.txt. The summary includes host version and binary fingerprint, plugin fingerprints, observed server version and authentication mode when available, selected categories, outcomes, operating settings, safe timings/counts, aliased item/file references, diagnostic codes and structural failure evidence. Events retain allowlisted operation, route and timing/count fields. Unknown labels and all identity values become consistent aliases within that packet. No lookup table is exported.

The summary states certificate verification as one fixed phrase: `verified`, "Certificate checks are relaxed for this profile" or "Not encrypted: plain http, no certificate". Certificate subject, issuer and fingerprint stay out of the packet. Runs made by earlier versions keep their stored category IDs; the packet shows the current IDs.

Credentials, endpoint, company/user names, business values, definitions, source code, request arguments, response bodies, server error text, original filenames/paths, downloaded-file hashes and raw messages/logs are omitted. Exact messages from the shipped static authored-message vocabulary may be included; formatted explanations containing runtime values are omitted. A structural projection can describe field types, row counts and repeated SysRowID counts without copying row values. A packet cannot replace confidential source inspection or prove the contents of files it intentionally excludes.

Export performs no network calls or native operations. It snapshots existing metadata and streams projected log records into a compressed ZIP on a blocking worker. Successful collection does not scan responses for additional diagnostic structure. Active log files are read only to the length observed at export; incomplete tail records are counted and omitted. The packet describes an observational snapshot, not an atomic server snapshot.

The CLI operation is diagnostics.export. The MCP tool is diagnostics_export. Both use arguments {"id":"run UUID","path":"absolute new ZIP path"}. Omit id for the latest command failure before a run starts. Herbert keeps that failure in memory only. Restarting Herbert clears it. Export saved runs through their run ID while their destination remains accessible. Older runs may lack server version, authentication or structured context.

## Technical details and next steps

Each diagnostic keeps its `code`, `message`, `retryable` and optional `identity` unchanged. The message says what happened and, where Herbert knows it, what to do next. Paths, IDs, lower-level errors and server text go in the diagnostic's `context` instead of the message.

- In the app, `cause`, `osError` and `serverMessage` appear under a collapsed **Technical details** section in error notices and run issues.
- MCP and CLI replies return the whole `context`. On an MCP error, `content[1]` adds the message and, when the message gives no step, a `Next step:` line. A step that asks you to save diagnostics also names `diagnostics_export`.
- A refused BAQ import (`ARTIFACT_IMPORT_REFUSED`) keeps Epicor's import log in `context.serverMessage` and its import decisions in `context.importDecisions`.
- An invalid page size returns `INVALID_SETTING` with its own message. Page size accepts 1 to 2,147,483,647, or blank for the default.

The sanitized packet applies its own allowlist to `context`. It never includes `cause` or server text, so share **Technical details** separately only when you are allowed to.

## Server-change outcomes

`run_status` reports `serverChanges` separately from collection completeness. `leaseLost` is true after `SERVER_LEASE_LOST`: new helper work stopped, while Direct reads and cleanup continued. `resources` lists `SERVER_LEASE_LOST_DURING_CALL` as `uncertain`, and `SERVER_CLEANUP_PENDING` and `CLEANUP_AUTHORITY_REQUIRED` as `pending`. `foreignPending` lists helpers this run kept: `active` (a recognized Herbert helper under another run's live lease, removed later once that lease ends), `recognized` (found when this run had no lease host) or `unrecognized` (looks like a Herbert helper but couldn't be confirmed). For `SERVER_CLEANUP_PENDING`, recover the run with `run_recover` to finish its cleanup. Herbert removes helpers left by an earlier Herbert version automatically when their code matches a released Herbert helper and no running Herbert holds them. The activity log records how many it removed. `CLEANUP_AUTHORITY_REQUIRED` means a helper looked like Herbert's but couldn't be confirmed, so Herbert kept it. Check it before removing it by hand.

## Selection evidence and privacy

The protected local run journal retains effective `pluginSettings`, module fingerprints and discovered identities. The journal also snapshots original plugin-declared filter metadata in `pluginSelectionContracts`, preserving the matching-contract description/version with the frozen values and fingerprint. Recovery uses these frozen values; `FROZEN_SELECTION_REQUIRED` reports a legacy journal whose missing filter cannot safely inherit current saved options. Existing module-hash mismatch refusal remains independent.

For UD codes, `codeTypeIdPrefix` uses matching contract `ud-codes-code-type-id-prefix-v1`: literal case-sensitive prefix, no trimming or normalization. Selection occurs locally after complete native-page validation. Server pages can include unselected roots; empty selected pages can have continuation. Final `verifiedAbsent` within completed discovery means no eligible items matched the run selection. It is distinct from invalid settings, a missing exact identity, denied reads and incomplete discovery. Counts describe only what the collector actually measures; a selected count is not an excluded or server-total count.

Exact IDs, filter values, native predicates and private selection objects remain local. The sanitized packet omits `pluginSettings`, `pluginSelectionContracts` and unallowlisted selection context; it retains allowed phase/route/page evidence and module fingerprints. Export adds no server calls. Inspect private frozen settings locally when diagnosing selection; share the sanitized ZIP for route/failure correlation.

## Collector repairs from field reports

UD mapping columns use native MapTableID/Seq ownership and do not require a nonexistent Company field. An explicit Company, if present, must agree. Report data-source parameter projections retain identical repeated rows and copied SysRowIDs, while contradictory rows with the same native four-part key are rejected. BPM code filenames distinguish directive owners and case-insensitive step collisions. Classic scripts with unknown language retain exact extracted text in .txt with a warning; original native data/XML remain preserved.

Stored report packages that fail ZIP/RDL verification retain their original bytes as .bin and are explicitly partial. The packet reports container signature, byte length and ZIP readability, never the payload. Native ReportSvc uses ZIP storage; the field report does not establish whether its unreadable payload is corrupted, encoded differently, or from a different server contract. Report style layout errors are attributed to affected style records.

Labels/Bartender bodies, EDI definition files, shared SSRS resources and external code assemblies use their own native acquisition and recovery routes. Native ReportSvc routes EDI definitions through company-data EDI_Definitions and handles Bartender locations separately from SSRS. Do not submit these paths to the SSRS reader or call a native generation/deployment operation merely to silence a diagnostic.

When a response produces several issues, one structural projection is retained for that response. The issues share an evidence alias in the packet, avoiding repeated scans and repeated copies of the same page structure. The observed native database version is read from the existing admission response, including its structured _Major/_Minor/_Build/_Revision form; packet export adds no version request.

Validated server diagnostic correlation UUIDs and a recognized response content type are retained on failed HTTP responses. These support server-log lookup without sharing the server message. A correlation candidate matching a known credential is rejected, including case and hyphen variations. Business record IDs remain aliased.
