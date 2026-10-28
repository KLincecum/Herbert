# Herbert: instructions for agents

Apply these instructions when you operate an installed Herbert package.
Read the installed [MCP guide](docs/MCP-GUIDE.md) for the complete command reference.

## Discover the installed capabilities

1. Initialize the MCP connection and send notifications/initialized.
2. Read tools/list, capabilities and help_read.
3. Use the installed schemas and plugin help for the requested collection.

The package includes mcp.claude-desktop.example.json for Claude Desktop’s mcpServers format.
Run herbert mcp-config to produce that format with the actual executable and local paths.
Merge the Herbert entry into the client’s existing configuration without replacing unrelated entries.
Other clients can require another configuration format.
A client does not necessarily read an adjacent AGENTS.md automatically.
Load this file explicitly or obtain it through help_read.

## Check account readiness and protect credentials

Epicor connections require the existing Security Manager admission checks.
An eligible account must be enabled and unlocked.
Read the readiness result before a native collection.
Treat unverified or failed verification as unresolved eligibility.
Report the supplied diagnostic and its required action.
Account eligibility does not authorize server changes.

Keep credentials outside command arguments, MCP configuration and logs.
Use streamed input, a selected user-authored environment file, or an explicitly selected secure-store entry.
Save credentials only when the user requests that action.
Do not publish profiles, endpoints, credentials or business output with the release.

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

1. Call serverChanges_preview with the effective selection and a stable requestId.
2. Read the possible effects, unresolved facts and matching authorizations.
3. Use an existing matching grant without requesting the same permission again.
4. If authority is missing, present the material effects and obtain explicit authorization.
5. Call serverChanges_authorize only for the approved preview, scope and lifetime.
6. Supply its authorizationId and explicit run permissions when you start the run.

A client acknowledgement does not prove that a person read the preview.
Do not create a grant from an ordinary download request.
Do not authorize an undefined or unresolved expansion.
A one-run grant follows that logical run’s stable identity and ordinary recovery.
A session grant ends with its defined connection session.
A workflow grant applies only to the explicitly named workflow and its permitted scope.
Grant creation does not create or activate a schedule.
A changed target, principal, capability expansion or relevant package can require new authority.
Harmless tuning and narrower permitted selections do not require repeated permission.

## Report collection and cleanup separately

An accepted start is not a completed run.
Poll run_status until execution finishes.
Read completeness, diagnostics and cleanup results before reporting completion.
Keep useful direct or partial output when another capability is unavailable.
Identify missing content and recovery limits accurately.

An owned change has an exact native identity and recorded ownership.
A lease release ends this run’s claim on a shared resource.
A lease release does not prove deletion.
Verified cleanup requires native evidence of removal or restoration.
Do not infer no changes or successful cleanup from the selected mode.
Report pending, failed or unknown cleanup with the exact identity and safe next action.
Preserve unrelated resources, preexisting memberships and other active owners.

After an uncertain write, inspect the original operation and native evidence before another write.
Use the supported recovery route and the original ownership and authorization context.
Do not repeat an uncertain installation, invocation or deletion blindly.
Revocation stops new optional changes but does not abandon proven owned cleanup.
