# Herbert: instructions for agents

Apply these instructions when you operate an installed Herbert package.
Read the installed [MCP guide](docs/MCP-GUIDE.md) for the complete command reference.

## Discover the installed capabilities

1. Initialize the MCP connection and send notifications/initialized.
2. Read tools/list, capabilities and help_read.
3. Use the installed schemas and plugin help for the requested collection.

MCP tool names are the application operation names with each dot replaced by an underscore, for example serverChanges_preview and run_retryFailed.
Use the category IDs that capabilities reports, such as ud-codes or baqs-and-ubaqs.
A retired category code from an earlier version is refused with LEGACY_CATEGORY_ID. Resend with the ID in error.context.replacementCategory.
A plugin's selection setting help ends with Matching contract: followed by its ID. Follow that help, and do not invent filter syntax.

The package includes mcp.claude-desktop.example.json for Claude Desktop’s mcpServers format.
Run herbert mcp-config to produce that format with the actual executable and local paths. Add --client codex, claude-code, gemini, vscode, cursor or generic for other clients' formats.
A tool approval in your client is not authority to change the Epicor server. Herbert's own preview and authorization steps still apply.
Merge the Herbert entry into the client’s existing configuration without replacing unrelated entries.
Other clients can require another configuration format.
A client does not necessarily read an adjacent AGENTS.md automatically.
Load this file explicitly or obtain it through help_read.

## Check account readiness and protect credentials

Epicor connections require the existing Security Manager admission checks.
An eligible account must be enabled and unlocked.
Read the readiness result before a native collection. A successful connection returns admission verifiedSecurityManager.
Treat unverified or failed verification as unresolved eligibility.
Report the supplied diagnostic and its required action.
Account eligibility does not authorize server changes.

Keep credentials outside command arguments, MCP configuration and logs.
Use streamed input, a selected user-authored environment file, or an explicitly selected secure-store entry.
Select an environment file entry with environmentId. The older environment_id spelling is still accepted.
Save credentials only when the user requests that action.
Do not publish profiles, endpoints, credentials or business output with the release.

## Choose the company and certificate trust

Every collection targets one company.
If the user did not name one, call connection_test without a company. It lists availableCompanies, then signs out.
Ask the user to choose, unless the task already identifies the company.
connection_connect and downloads without a company are refused with COMPANY_REQUIRED.
profiles_upsert without a company saves the profile and warns COMPANY_NOT_SET.

Set allowUntrustedCertificate only when the user explicitly asks for it. Never infer it from a CERTIFICATE_UNTRUSTED error.
It relaxes only the unknown-issuer check. The server name and certificate dates are still checked.
It pins nothing, and any certificate the server presents can act as the anchor. Use it only for servers the user controls.
Report transportTrust.mode with the result: verified, relaxedUntrustedIssuer or unencrypted.

## Read errors

On failure, act on error.code, not the message wording.
error.message says what happened and usually what to do next. error.context holds the technical details.
For clients that show text, content[1] repeats the message with a Next step line.
Follow that step, or report it to the user when it needs a human decision.

## Obtain the correct server-change authority

A helper is a declared native component that Herbert installs or invokes for selected content.
Temporary access is an eligible group-membership change for the authenticated Epicor user.
A grant records explicit authority for defined server changes and a defined lifetime.

Omitted run permissions mean Direct collection and temporary access off.
Direct excludes helper installation and invocation.
Helper permission and temporary-access permission are independent.
Direct can use temporary access when the run explicitly permits and authorizes that change.
Some native exports create temporary server files and need their declared authority independently of helpers.
A saved preference or an eligible account is not a grant.

1. Call serverChanges_preview with fullBackup and run, the effective start arguments including a stable requestId.
2. Read the possible effects, unresolved facts and matching authorizations. If authorizationRequired is false, start without an authorization.
3. Use an existing matching grant without requesting the same permission again.
4. If unresolved is not empty, do not authorize. Narrow the request or use Direct without temporary access.
5. If authority is missing, check the human authorization already present in the current task.
6. If it covers the preview’s target, material effects and lifetime, create the supported host grant.
7. Otherwise, present the material effects and obtain explicit authorization.
8. Call serverChanges_authorize only for the approved previewId, materialDigest and lifetime (oneRun, session or workflow).
9. Pass the returned authorization.id as authorizationId, with explicit run permissions, to backup_start or retrieve_start.

A client acknowledgement does not prove that a person read the preview.
Do not create a grant from an ordinary download request.
Do not authorize an undefined or unresolved expansion.
A one-run grant follows that logical run’s stable identity and ordinary recovery.
A session grant ends with its defined connection session.
A workflow grant applies only to the explicitly named workflow and its permitted scope.
Grant creation does not create or activate a schedule.
A changed target, principal, capability expansion or relevant package can require new authority.
Harmless tuning and narrower permitted selections do not require repeated permission.
Use serverChanges_list and serverChanges_inspect to review grants. Use serverChanges_revoke when the user withdraws one.

## Report collection and cleanup separately

An accepted start is not a completed run.
Poll run_status until executionActive is false.
Read completeness, diagnostics and cleanup results before reporting completion.
Keep useful direct or partial output when another capability is unavailable.
Identify missing content and recovery limits accurately.

An owned change has an exact native identity and recorded ownership.
A lease release ends this run’s claim on a shared resource.
A lease release does not prove deletion.
Verified cleanup requires native evidence of removal or restoration.
Do not infer no changes or successful cleanup from the selected mode.
Read run_status serverChanges: leaseLost, resources and foreignPending.
After a lost lease, new helper work stops and direct reads continue. Report the affected items as incomplete.
Cleanup removes only resources the run owns. Report foreignPending helpers. Do not remove them.
Report pending, failed or unknown cleanup with the exact identity and safe next action.
Preserve unrelated resources, preexisting memberships and other active owners.

After an uncertain write, inspect the original operation and native evidence before another write.
Use the supported recovery route and the original ownership and authorization context.
Do not repeat an uncertain installation, invocation or deletion blindly.
Revocation stops new optional changes but does not abandon proven owned cleanup.
