//! Collection-specific omissions belong to these plugins, before raw retention.
use herbert_contract::{Diagnostic, GuestRequest, NativeResponsePolicy, NativeRetentionRule, NativeRowPredicate, NativeTablePolicy, ParameterRedaction, PluginSetting, PluginSettingOption};

pub const SENSITIVE_FILTER_SETTING: &str = "filterSensitiveFields";
pub const CODE_TYPE_PREFIX_SETTING: &str = "codeTypeIdPrefix";
pub const QUERY_PREFIX_SETTING: &str = "queryIdPrefix";
pub const LIBRARY_PREFIX_SETTING: &str = "libraryIdPrefix";
pub const SCHEMA_FAMILY_SETTING: &str = "definitionFamily";
pub const SCHEMA_PREFIX_SETTING: &str = "schemaTableIdPrefix";
pub const PROPERTY_PREFIX_SETTING: &str = "tableIdPrefix";
pub const DIRECTIVE_FAMILY_SETTING: &str = "directiveFamily";
pub const METHOD_PREFIX_SETTING: &str = "methodCodePrefix";
pub const LAYER_PREFIX_SETTING: &str = "layerNamePrefix";
pub const FORM_PREFIX_SETTING: &str = "formNamePrefix";
pub const USER_PREFIX_SETTING: &str = "userIdPrefix";
pub const DASHBOARD_PREFIX_SETTING: &str = "dashboardIdPrefix";
pub const APPLICATION_PREFIX_SETTING: &str = "applicationIdPrefix";
pub const SEARCH_PREFIX_SETTING: &str = "searchIdPrefix";
pub const SEARCH_FAMILY_SETTING: &str = "definitionFamily";
pub const MENU_PREFIX_SETTING: &str = "menuIdPrefix";
pub const REPORT_DEFINITION_PREFIX_SETTING: &str = "definitionIdPrefix";
pub const REPORT_ID_PREFIX_SETTING: &str = "reportIdPrefix";
pub const PACKAGE_PATH_PREFIX_SETTING: &str = "packagePathPrefix";
pub fn sensitive_filter_enabled(request: &GuestRequest) -> bool {
    request.state.pointer("/_pluginSettings/filterSensitiveFields")
        .or_else(|| request.state.pointer("/_pluginState/config/filterSensitiveFields"))
        .and_then(serde_json::Value::as_bool).unwrap_or(false)
}
/// Describe saved options as the effective defaults for a new host session.
/// The host must not need to understand this plugin's private state format.
pub fn configured_settings(category: &str, request: &GuestRequest) -> Result<Vec<PluginSetting>, Diagnostic> {
    let mut declarations = settings(category, sensitive_filter_enabled(request));
    let mut saved = request.state["_pluginState"]["config"].clone();
    // The host migrates this retired shared setting before starting work.
    if category == "functions" { if let Some(config) = saved.as_object_mut() { config.remove("localDecompilerWorkers"); } }
    let values = herbert_contract::resolve_plugin_settings(&declarations, &saved)?;
    #[cfg(feature="function-compiled")]
    let values={let mut values=values;if category=="functions"{crate::function_compiled::normalize(&mut values);}values};
    #[cfg(feature="ordinary-bpm-compiled")]
    let values={let mut values=values;if category=="bpm-directives"{crate::bpm_compiled::normalize(&mut values);}values};
    #[cfg(feature="ubaq-compiled")]
    let values={let mut values=values;if category=="baqs-and-ubaqs"{crate::ubaq_compiled::normalize(&mut values);}values};
    for declaration in &mut declarations {
        match declaration {
            PluginSetting::Integer { key, default, .. } => *default = values[key.as_str()].as_u64().unwrap() as u32,
            PluginSetting::Checkbox { key, default, .. } => *default = values[key.as_str()].as_bool().unwrap(),
            PluginSetting::Choice { key, default, .. } => *default = values[key.as_str()].clone(),
            PluginSetting::Filter { key, default, .. } => *default = values[key.as_str()].as_str().unwrap().to_owned(),
        }
    }
    Ok(declarations)
}

pub fn settings(category: &str, enabled: bool) -> Vec<PluginSetting> {
    if category=="access-and-assignments" {return vec![PluginSetting::choice("accessFamily","Access family","all",[("All access and assignments","all"),("Security definitions","definitions"),("User assignments","userAssignments"),("API-key scope assignments","apiKeyAssignments")].map(|(label,value)|PluginSettingOption{label:label.into(),value:value.into()})).description("Choose complete existing security definition surfaces, all three user-assignment projections, or API-key scope assignments. All is the default. Acquired validation/errors, source-only supplements and scope/dependency context remain intact. Exact acquisition ignores discovery controls. Matching contract: access-and-assignments-access-family-v1."),PluginSetting::filter("userIdPrefix","User ID starts with","").description("Literal, case-sensitive native UserID prefix on user access, user-company and user-external-company assignments. Every character is preserved; blank selects all eligible users. Security definitions and API-key assignments remain independent and unaffected. Choose User assignments to collect only matching user assignments. Herbert narrows the three user-assignment reads to user IDs beginning with the prefix's characters before any %, _, [, ] or non-ASCII character, then applies the literal prefix locally. Only selected rows validate; unselected rows produce no diagnostics. Complete projections and raw count/keyset continuation over the narrowed read remain unchanged. Exact acquisition ignores this prefix. Matching contract: access-and-assignments-user-id-prefix-v1.")];}
    if category=="automation-and-integrations" {let mut declarations=vec![PluginSetting::choice("automationFamily","Automation family","all",[("All automation","all"),("System agents and schedules","systemAgents"),("Process sets","processSets"),("Message templates","messageTemplates"),("Global alerts","globalAlerts")].map(|(label,value)|PluginSettingOption{label:label.into(),value:value.into()})).description("Choose one complete existing automation family. All is the default. Selected schedules/tasks/parameters, shared owners, raw visibility/manifest validation and whole-family native fallback remain intact. Acquired errors stay incomplete even when output is excluded. No task, message or alert is executed. Exact native acquisition ignores this control. Matching contract: automation-and-integrations-automation-family-v1.")];declarations.push(PluginSetting::checkbox(SENSITIVE_FILTER_SETTING, "Filter sensitive fields and parameters", enabled).description("Off by default: preserve native field and parameter values. When enabled, omit declared password, secret, API key and token values from this plugin’s collected definitions. System artifact exclusions still apply."));return declarations;}
    if category=="custom-resources" {return vec![PluginSetting::choice("resourceFamily","Resource family","all",[("All resources","all"),("Themes and theme overrides","themes"),("Translation library","translations"),("Image library","images"),("Customization FileStore resources","fileStore")].map(|(label,value)|PluginSettingOption {label:label.into(),value:value.into()})).description("Choose complete existing resource families. Themes includes standalone overrides under existing themes. Translation and image libraries remain whole aggregate graphs; no child-ID prefix is applied. All is the default. Acquired validation/errors and raw stream continuation remain; FileStore ownership and original bytes are unchanged. Exact acquisition remains unsupported. Matching contract: custom-resources-resource-family-v1.")];}
    if category=="programs-and-extensions" {return vec![PluginSetting::choice("definitionFamily","Definition family","all",[("All programs/extensions","all"),("Services","services"),("DataSets","datasets"),("Extension sets","extensions"),("External assemblies","externals")].map(|(label,value)|PluginSettingOption {label:label.into(),value:value.into()})).description("Choose complete existing program families. All is the default. Native and acquired supplemental metadata validate before local selection. Direct partial coverage survives helper refusal. Exact acquisition ignores discovery settings. Matching contract: programs-and-extensions-definition-family-v1."),PluginSetting::filter("definitionIdPrefix","Definition ID starts with","").description("Literal, case-sensitive native ClassName, DataSetID, ExtensionSetID or external Name prefix in selected families. Every character is preserved; blank selects all. SystemCode variants, complete relationships/hidden context and required original binaries/symbols stay together. Selection precedes per-owner binary reads; acquired excluded failures stay incomplete. Exact acquisition ignores this prefix. Matching contract: programs-and-extensions-definition-id-prefix-v1.")];}
    if category=="solutions-and-packages" {return vec![PluginSetting::choice("definitionFamily","Definition family","all",[("All solution sources","all"),("Solution definitions","solutions"),("Solution types","solutionTypes"),("Retained installed packages","retainedPackages")].map(|(label,value)|PluginSettingOption {label:label.into(),value:value.into()})).description("Choose complete persisted solution/type graphs or independent retained package sources. All is the default. Selected solutions keep required type context even when standalone type output is excluded. Acquired graph/catalog errors remain before selection. Exact acquisition ignores discovery settings. Matching contract: solutions-and-packages-definition-family-v1."),PluginSetting::filter("solutionIdPrefix","Solution ID starts with","").description("Literal, case-sensitive native SolutionID prefix independently applied to persisted headers and retained installed-package catalog owners. Every character is preserved; blank selects all. Standalone custom solution types are unaffected by this prefix. Complete selected membership/tracker/type context and original retained ZIP bytes remain intact. Exact acquisition ignores this prefix. Matching contract: solutions-and-packages-solution-id-prefix-v1.")];}
    if matches!(category,"configurators"|"business-rules-and-mappings") {
        let configurators=category=="configurators";
        return vec![PluginSetting::integer("itemsPerPage",if configurators{"Definitions per request"}else{"Forms / Structured Output per page"},100,1,i32::MAX as u32).description(if configurators{"All native pages are collected; complete selected configurator graphs and required resources stay together. This editable default is not a total limit."}else{"All native forms and Structured Output pages are collected. Posting reads the complete company graph independently of this editable page setting."}),
            PluginSetting::choice("definitionFamily","Definition family","all",if configurators{vec![("All configurator sources","all"),("Configurators","configurators"),("Lookup tables","lookups"),("Persisted rule and variable source","persistedSources")]}else{vec![("All business definitions","all"),("BPM data forms","forms"),("Structured Output","structuredOutput"),("Posting rules","postingRules")]}.into_iter().map(|(label,value)|PluginSettingOption {label:label.into(),value:value.into()})).description(if configurators{"Select existing complete native families; default includes configurators, lookups and all five independent persisted rule/variable sources. Exact acquisition ignores discovery choices. Matching contract: configurators-definition-family-v1."}else{"Select complete forms, Structured Output or posting definitions; default includes all three. Full acquired graph validation and errors remain before selection. Exact acquisition ignores discovery choices. Matching contract: business-rules-and-mappings-definition-family-v1."}),
            PluginSetting::filter("definitionIdPrefix","Definition ID starts with","").description(if configurators{"Literal, case-sensitive ConfigID or LookupTblID prefix within selected families. Every character is preserved; blank selects all. Independent Company+SysRowID persisted sources are unaffected by this prefix. Required children, shared context and resources stay with selected roots. Acquired pages validate before selection; raw continuation and errors remain. Exact acquisition ignores discovery settings. Matching contract: configurators-definition-id-prefix-v1."}else{"Literal, case-sensitive FormId or RptStructuredOutputDefID prefix within selected families. Every character is preserved; blank selects all. Integer ACTTypeUID posting graphs are independent of this prefix. Discovery reads ask the server for a broad LIKE superset matching the prefix text before any %, _, [, ] or non-ASCII character; Herbert applies the literal prefix locally and validates only selected definitions, so definitions the prefix excludes are not validated or reported. Raw continuation remains. Complete selected children/maps stay together. Exact acquisition ignores discovery settings. Matching contract: business-rules-and-mappings-definition-id-prefix-v1."})];
    }
    if matches!(category, "classic-customizations" | "classic-personalizations") {
        let (key,label,description,page_label) = if category == "classic-customizations" {
            (LAYER_PREFIX_SETTING,"Layer name starts with","Literal, case-sensitive Key1 layer-name prefix across all eligible Classic layer types. Every character is preserved; blank selects all. AND with Form name starts with. Epicor returns a broad superset matching the prefix text before any %, _, [, ] or non-ASCII character. Identities and Content type/UTF-16 length are validated only for selected definitions; definitions the literal prefixes exclude are neither validated nor reported. Exact retrieval ignores these discovery prefixes. Matching contract: classic-customizations-layer-name-prefix-v1.","Customizations per request")
        } else {
            (USER_PREFIX_SETTING,"User ID starts with","Literal, case-sensitive Key1 user-ID prefix for Classic personalizations. Every character is preserved; blank selects all eligible owners. AND with Form name starts with. Epicor returns a broad superset matching the prefix text before any %, _, [, ] or non-ASCII character. Identities and Content type/UTF-16 length are validated only for selected definitions; definitions the literal prefixes exclude are neither validated nor reported. Exact retrieval ignores these discovery prefixes. Matching contract: classic-personalizations-user-id-prefix-v1.","Personalizations per request")
        };
        return vec![
            PluginSetting::integer("itemsPerPage",page_label,100,1,i32::MAX as u32)
                .description("The effective editable default is 100 definitions per request. All native pages are collected; selected-empty pages continue. Tested page sizes are not supported maxima."),
            PluginSetting::filter(key,label,"").description(description),
            PluginSetting::filter(FORM_PREFIX_SETTING,"Form name starts with","")
                .description(if category == "classic-customizations" {"Literal, case-sensitive Key2 native form-key prefix, not a translated caption. The text before any wildcard or non-ASCII character also narrows the server read. Spaces, punctuation and Unicode remain unchanged. Blank selects all eligible forms; AND with layer name. All seven native keys and complete selected XML/scripts remain intact. Exact acquisition ignores this filter. Matching contract: classic-customizations-form-name-prefix-v1."} else {"Literal, case-sensitive Key2 native form-key prefix, not a translated caption. The text before any wildcard or non-ASCII character also narrows the server read. Spaces, punctuation and Unicode remain unchanged. Blank selects all eligible forms; AND with user ID. Full personal owner/composite identity and complete selected XML/scripts remain intact. Exact acquisition ignores this filter. Matching contract: classic-personalizations-form-name-prefix-v1."}),
        ];
    }
    if category == "ud-schema" {
        return vec![
            PluginSetting::integer("itemsPerPage", "Tables per request", 500, 1, i32::MAX as u32)
                .description("The editable default remains 500 parent rows per request. All pages are collected."),
            PluginSetting::choice(SCHEMA_FAMILY_SETTING, "Definition family", "schemaAndAllMappings", [
                ("Schema tables", "schema"), ("UD mappings", "mappings"),
                ("Schema tables and all UD mappings", "schemaAndAllMappings"),
            ].map(|(label, value)| PluginSettingOption { label: label.into(), value: value.into() }))
                .description("Choose schema tables, UD mappings, or both. The combined choice includes all eligible UD mappings independently of the schema prefix. Exact identity retrieval ignores this discovery setting. Matching contract: ud-schema-definition-family-v1."),
            PluginSetting::filter(SCHEMA_PREFIX_SETTING, "Schema table ID starts with", "")
                .description("Literal, case-sensitive DataTableID prefix for schema tables only. Every entered character, including spaces, is preserved. Blank selects all eligible schema tables. Complete custom children and stock-parent key context stay together. All acquired rows validate before local selection; native paging continues through empty selected pages. UD mappings are independent of this prefix. Exact identity retrieval ignores it. Matching contract: ud-schema-schema-table-id-prefix-v1."),
        ];
    }
    if category == "extended-properties" {
        return vec![PluginSetting::filter(PROPERTY_PREFIX_SETTING, "Table ID starts with", "")
            .description("Literal, case-sensitive DataTableID prefix. Every entered character, including spaces, is preserved. Blank selects all eligible property tables. The required helper still reads all global custom properties; Herbert validates every table and selects complete graphs locally. Direct only saves field overrides only and marks each table Partial. Exact identity retrieval ignores this discovery filter. Matching contract: extended-properties-table-id-prefix-v1.")];
    }
    if category == "ud-codes" {
        return vec![
            PluginSetting::integer("itemsPerPage", "Code types per request", 500, 1, i32::MAX as u32)
                .description("The editable default remains 500 code types per request. This is an editable default, not a maximum. Herbert collects all pages."),
            PluginSetting::filter(CODE_TYPE_PREFIX_SETTING, "Code type ID starts with", "")
                .description("Literal, case-sensitive prefix. Every entered character, including spaces, is preserved. Blank selects all eligible code types. Herbert reads native pages and selects complete types and their entries locally. Exact identity retrieval ignores this discovery filter. Matching contract: ud-codes-code-type-id-prefix-v1."),
        ];
    }
    if category=="bpm-directives"{
        let settings = vec![
        PluginSetting::integer("itemsPerPage","Methods per request",100,1,i32::MAX as u32).description("The editable default is 100 methods per request. All pages are collected."),
        PluginSetting::choice(DIRECTIVE_FAMILY_SETTING, "Directive family", "methodAndData", [
            ("Method and data directives", "methodAndData"), ("Method directives", "method"), ("Data directives", "data"),
        ].map(|(label, value)| PluginSettingOption { label:label.into(), value:value.into() }))
            .description("Choose BO method groups, DB data groups, or both. Every selected data group retains both standard and in-transaction directives. Exact acquisition ignores this discovery setting. Matching contract: bpm-directives-directive-family-v1."),
        PluginSetting::filter(METHOD_PREFIX_SETTING, "Method or table code starts with", "")
            .description("Literal, case-sensitive prefix of the full native BpMethodCode, such as Ice.BO.X.Update or Ice.UD39.Update. Every entered character, including spaces, is preserved. Blank selects all eligible owners in the chosen families. Herbert narrows the native read to codes beginning with the prefix's characters before any %, _, [, ] or non-ASCII character, then applies the literal prefix locally. Only selected owners and their rows validate; unselected rows produce no diagnostics. Native paging continues through empty selections. Exact acquisition ignores this filter. Required native dependency packages retain their wider service/table scope. Matching contract: bpm-directives-method-code-prefix-v1."),
        ];
        #[cfg(feature="ordinary-bpm-compiled")]
        let settings = {let mut settings=settings;settings.extend([
        PluginSetting::checkbox(crate::bpm_compiled::DOWNLOAD,"Download compiled assemblies",false).description("Preserve associated original DLL/PDB bytes under Herbert Data/raw. Keep the whole backup and its journal. Capture requires the scoped read helper. Normal BPM output remains independent."),
        PluginSetting::checkbox(crate::bpm_compiled::DECOMPILE,"Create decompiled C# projects",false).description("Also capture original assemblies. Create local projects under their BPM owners. Data-trigger projects contain both directive types. Missing dependencies and reconstruction limits remain explicit.")
        ]);settings};
        return settings;
    }
    #[cfg(feature="function-compiled")]
    if category=="functions" {return vec![
        PluginSetting::integer("itemsPerPage","Libraries per request",100,1,i32::MAX as u32).description("The editable default is 100 libraries per request. All pages are collected."),
        PluginSetting::filter(LIBRARY_PREFIX_SETTING, "Library ID starts with", "")
            .description("Literal, case-sensitive prefix. Every entered character, including spaces, is preserved. Blank selects all eligible libraries. Herbert selects validated inventory IDs locally, then reads complete libraries in bulk. Exact identity retrieval ignores this discovery filter. Matching contract: functions-library-id-prefix-v1."),
        PluginSetting::checkbox(crate::function_compiled::DOWNLOAD,"Download compiled assemblies",false).description("Preserve associated original DLLs and available symbols under Herbert Data/raw. Keep the whole backup and journal. Exact store access requires the optional scoped reader."),
        PluginSetting::checkbox(crate::function_compiled::DECOMPILE,"Create decompiled C# projects",false).description("Also capture compiled assemblies. Reconstruct local C# projects. References are not downloaded. Reconstructed source may be partial and may not build."),
    ];}
    if category == "functions" {
        return vec![
            PluginSetting::integer("itemsPerPage", "Libraries per request", 100, 1, i32::MAX as u32)
                .description("The editable default is 100 libraries per request. All pages are collected."),
            PluginSetting::filter(LIBRARY_PREFIX_SETTING, "Library ID starts with", "")
                .description("Literal, case-sensitive prefix. Every entered character, including spaces, is preserved. Blank selects all eligible libraries. Herbert selects validated inventory IDs locally, then reads complete libraries in bulk. Exact identity retrieval ignores this discovery filter. Matching contract: functions-library-id-prefix-v1."),
        ];
    }
    if category=="business-rules-and-mappings"{
        // Retain the tested native forms/Structured Output page default.
        // Posting reads the whole company graph; it has no root/child windows.
        return vec![PluginSetting::integer("itemsPerPage","Forms / Structured Output per page",100,1,i32::MAX as u32).description("Native forms and Structured Output page size. Posting reads the complete in-scope company graph up front, independently of this setting. All native pages are collected.")];
    }
    if matches!(category, "report-styles-and-layouts" | "report-behavior") {
        // Starting page size; editable.
        let mut options=vec![PluginSetting::integer("itemsPerPage", "Report rows per request", 100, 1, i32::MAX as u32)
            .description("Editable page size for native report graphs. All pages are collected."),
            PluginSetting::filter(REPORT_ID_PREFIX_SETTING,"Report ID starts with","")
                .description(if category=="report-styles-and-layouts" {"Literal, case-sensitive native ReportID prefix for complete eligible style/layout/image owners. Every character is preserved; blank selects all. Required subreports/images remain independent of prefixes. Stored current/history packages are independent of this filter. Acquired visibility validates before local selection and physical reads. Exact acquisition ignores discovery filters. Matching contract: report-styles-and-layouts-report-id-prefix-v1."}else{"Literal, case-sensitive native ReportID prefix for complete eligible behavior style owners. Every character is preserved; blank selects all. All saved disabled/enabled rule bodies, widget text and required style/image context stay together. Acquired visibility validates before selection and physical reads. Exact acquisition ignores this filter. Matching contract: report-behavior-report-id-prefix-v1."})];
        if category=="report-styles-and-layouts" {options.push(PluginSetting::filter(PACKAGE_PATH_PREFIX_SETTING,"Stored package path starts with","")
            .description("Literal, case-sensitive prefix over full stored PrintProgram for current packages and all retained versions. Every character is preserved; blank selects all. Independent of style ReportID and deployed layout/subreport/image dependencies. Native path/tenant authorization remains unchanged. Retained-version discovery asks Epicor for a broad superset of paths starting with the prefix text before any %, _, [, ] or non-ASCII character; current packages are read in full. Herbert applies the literal prefix locally and validates only selected packages; excluded packages are neither validated nor reported. A row without a stored path cannot be matched and still fails. Selected invalid ZIPs retain original .bin bytes. Exact current/history acquisition ignores discovery filters. Matching contract: report-styles-and-layouts-package-path-prefix-v1."));}
        return options;
    }
    if category == "report-definitions" {
        // Full native RDD pages at 50 measured 8.67 MB on Kinetic 2026.1.
        return vec![PluginSetting::integer("itemsPerPage", "Report definitions per request", 50, 1, i32::MAX as u32)
            .description("Collect complete report definitions and their children. All pages are collected."),
            PluginSetting::choice("definitionFamily","Definition family","reportDataAndBaqReports",[("Report data definitions","reportData"),("BAQ reports","baqReports"),("Report data definitions and BAQ reports","reportDataAndBaqReports")].map(|(label,value)|PluginSettingOption {label:label.into(),value:value.into()}))
                .description("Choose complete RDD or BAQ-report definitions, or both (default). Full acquired child graphs and query-backed field occurrences validate before selection. Exact acquisition ignores discovery settings. Matching contract: report-definitions-definition-family-v1."),
            PluginSetting::filter(REPORT_DEFINITION_PREFIX_SETTING,"Definition ID starts with","")
                .description("Literal, case-sensitive native RptDefID or BAQRptID prefix in the chosen families. Every character is preserved; blank selects all. Full acquired graphs and typed output validate before local selection; raw paging/source continuation and incomplete diagnostics remain. Exact acquisition ignores this filter. Matching contract: report-definitions-definition-id-prefix-v1.")];
    }
    if category == "menus-and-launch-definitions" {
        // Both tested versions returned identical full rows at 100, 1000 and 4000.
        // 4000 reduced calls with responses below 2 MB.
        return vec![PluginSetting::integer("itemsPerPage", "Menu rows per request", 4000, 1, i32::MAX as u32)
            .description("Editable page size. Custom menus have individual XML files; system state is saved in page snapshots. All pages are collected."),
            PluginSetting::filter(MENU_PREFIX_SETTING,"Menu ID starts with","")
                .description("Literal, case-sensitive native MenuID prefix for custom menus and system snapshot rows. Every character is preserved; blank selects all eligible menus. Required authorized custom ancestors retain complete source/XML as dependencies even outside the prefix; system parents remain target prerequisites. No descendants or unrelated branches are selected. Native graph row/revision witnesses validate ancestor body reads; missing, changed, unauthorized or ambiguous parents and relevant cycles keep closure incomplete. Exact Company+MenuID acquisition ignores this discovery setting. Matching contract: menus-and-launch-definitions-menu-id-prefix-v1.")];
    }
    if category == "searches-and-navigation" {
        // Testing showed 1,000 preserves identical files and removes
        // page round trips at modest memory cost.
        return vec![PluginSetting::integer("itemsPerPage", "Search definitions per request", 1000, 1, i32::MAX as u32)
            .description("Editable page size. Criteria and navigation children stay with their parent. All pages are collected."),
            PluginSetting::choice(SEARCH_FAMILY_SETTING,"Definition family","all",[
                ("All searches and navigation","all"),("Quick searches","quickSearch"),("Predictive searches","predictiveSearch"),("Named searches","namedSearch"),("Enterprise searches","enterpriseSearch"),("Saved context menus","savedContextMenu"),("Legacy context menus","legacyContextMenu"),
            ].map(|(label,value)|PluginSettingOption {label:label.into(),value:value.into()}))
                .description("Choose complete native families. Quick and Predictive use the stored IsPredictive flag. Default includes every existing family. Exact acquisition ignores discovery settings. Matching contract: searches-and-navigation-definition-family-v1."),
            PluginSetting::filter(SEARCH_PREFIX_SETTING,"Search ID starts with","")
                .description("Literal, case-sensitive native QuickSearchID, NSId or SearchID prefix for Quick/Predictive, Named and Enterprise searches. Spaces, punctuation and Unicode remain unchanged; blank selects all eligible IDs. Saved and legacy context menus are independent of this prefix. Complete children stay with selected roots; Named and Enterprise reads are narrowed to IDs starting with the prefix text before any %, _, [, ] or non-ASCII character; Herbert applies the literal prefix locally and validates and reports only selected searches. Quick/Predictive and context-menu sources are read whole and filtered locally; excluded rows are neither validated nor reported. Raw continuation remains. Exact acquisition ignores this setting. Matching contract: searches-and-navigation-search-id-prefix-v1.")];
    }
    if category == "kinetic-personalizations" {
        return vec![
            PluginSetting::filter(USER_PREFIX_SETTING, "User ID starts with", "")
                .description("Literal, case-sensitive native personal-owner prefix. Every character is preserved; blank selects all eligible owners and shared layouts. Nonblank selects personal owners only; ownerless shared layouts are excluded as standalone records. Required bases and original images of selected owners remain dependencies. Full acquired pages validate before local selection; raw paging and incomplete diagnostics remain. Exact acquisition ignores this discovery filter. Matching contract: kinetic-personalizations-user-id-prefix-v1."),
            PluginSetting::integer("itemsPerPage", "Personalization records per request", 100, 1, i32::MAX as u32)
                .description("Editable page size. All pages are collected."),
            PluginSetting::checkbox("combinedRead", "Combine source reads in one BAQ", false)
                .description("Read personalization source groups together with an unsaved BAQ. File dependencies still use their native readers. Your choice is saved.")
        ];
    }
    if category == "kinetic-applications-and-layers" {
        return vec![
            PluginSetting::filter(APPLICATION_PREFIX_SETTING, "Application ID starts with", "")
                .description("Literal, case-sensitive complete ViewId prefix. Every character is preserved; blank selects all eligible applications. Metadata validates before local selection and application/history offsets. Selected applications retain every layer, required parent, saved draft, resource and requested history revision. Raw metadata paging continues through empty selections. Exact application-bundle acquisition ignores this discovery filter. Matching contract: kinetic-applications-and-layers-application-id-prefix-v1."),
            PluginSetting::checkbox("includeVersionHistory", "Include version history", true)
                .description("Collect older saved revisions. Turn this off for current published definitions and saved drafts only. Your choice is saved with this plugin."),
            PluginSetting::integer("itemsPerPage", "Applications per batch / definition rows per request", 100, 1, i32::MAX as u32)
                .description("All pages are collected. A complete application can require several definition pages before its ZIP is ready."),
            PluginSetting::integer("metadataRowsPerPage", "Identity rows per request", 1000, 1, i32::MAX as u32)
                .description("Identity-only rows are small. A separate larger page avoids delaying downloads with many small inventory calls. This is an editable default, not a total limit.")
        ];
    }
    if category == "classic-dashboards" {
        return vec![
            PluginSetting::integer("itemsPerPage", "Dashboards per request", 16, 1, i32::MAX as u32)
                .description("Start at 16: a measured page peaked at 14.1 MB. This is an editable default, not a limit."),
            PluginSetting::filter(DASHBOARD_PREFIX_SETTING,"Dashboard ID starts with","")
                .description("Literal, case-sensitive DefinitionID prefix. Every entered character is preserved. Blank selects all eligible dashboards. Herbert validates the complete inventory page before selecting composite identities; selected-empty pages continue without definition/BAQ reads. All selected variants and complete children remain together. Referenced BAQs come from selected schemas only. Exact acquisition ignores this discovery filter. Matching contract: classic-dashboards-dashboard-id-prefix-v1."),
            PluginSetting::checkbox("includeReferencedBAQs", "Embed referenced BAQs", true)
                .description("Include native BAQ exports in each DBD, as Classic dashboard export does. Disable for definition-only files.")
        ];
    }
    if category == "baqs-and-ubaqs" {
        // GetRows uses positive Int32 values. The default is measured, not a cap.
        let settings=vec![PluginSetting::integer("queriesPerPage", "Queries per request", 100, 1, i32::MAX as u32)
            .description("The editable default is 100 query headers per request. Child rows determine response size. Larger pages can delay initial output."),
            PluginSetting::filter(QUERY_PREFIX_SETTING, "Query ID starts with", "")
                .description("Literal, case-sensitive prefix for BAQs and UBAQs. Every entered character, including spaces, is preserved. Blank selects all eligible queries. Epicor returns a broad superset matching the prefix text before any %, _, [, ] or non-ASCII character. Herbert selects whole queries locally with their children, diagrams and required dependencies, validating only selected queries; rows it cannot attribute to a query still fail the page. Native paging continues through empty selected pages. Exact identity retrieval is unsupported for BAQs and UBAQs. Matching contract: baqs-and-ubaqs-query-id-prefix-v1.")];
        #[cfg(feature="ubaq-compiled")]
        let settings={let mut settings=settings;settings.extend([
            PluginSetting::checkbox(crate::ubaq_compiled::DOWNLOAD,"Download compiled UBAQ BPM assemblies",false).description("Preserve original DLL/PDB bytes under Herbert Data/raw with exact query and method associations. Keep the whole backup and journal. Capture requires a read-only helper. Normal BAQ output remains independent."),
            PluginSetting::checkbox(crate::ubaq_compiled::DECOMPILE,"Create decompiled UBAQ BPM C# projects",false).description("Also capture original assemblies. Create local projects under each UBAQ method. References are not downloaded. Reconstructed source may be partial and may not build. Native recovery uses the BAQ package.")
        ]);settings};
        return settings;
    }
    let page = match category {
        "kinetic-applications-and-layers" => Some((100, "Kinetic applications per batch")),
        "kinetic-personalizations" => Some((100, "Personalization records per request")),
        _ => None,
    };
    if let Some((size, label)) = page {
        // These are operating defaults, not collection limits. Native paging
        // takes a positive Int32. Evidence and unmeasured cases are documented.
        return vec![PluginSetting::integer("itemsPerPage", label, size, 1, i32::MAX as u32)
            .description(if category == "ud-codes" { "The editable default remains 500 code types per request. This is an editable default, not a maximum. Herbert collects all pages." } else if category == "ud-schema" { "The editable default remains 500 parent rows per request. All pages are collected." } else if category == "bpm-directives" { "The editable default is 100 methods per request. All pages are collected." } else if category == "functions" { "The editable default is 100 libraries per request. All pages are collected." } else if category == "classic-customizations" { "The effective editable default is 100 customizations per request. All pages are collected." } else { "Editable page size. All pages are collected." })];
    }
    vec![]
}

#[cfg(test)]
mod saved_options_tests {
    use super::*;
    use herbert_contract::{GuestReply, PLUGIN_ABI, resolve_plugin_settings};
    use serde_json::json;


    #[test]
    fn automation_access_controls_preserve_defaults_sensitive_projection_and_saved_literal_user_prefix(){for(category,key,values)in[("automation-and-integrations","automationFamily",vec!["all","systemAgents","processSets","messageTemplates","globalAlerts"]),("access-and-assignments","accessFamily",vec!["all","definitions","userAssignments","apiKeyAssignments"])]{let declarations=settings(category,false);let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();assert_eq!(defaults[key],"all");assert!(defaults.get("itemsPerPage").is_none());let PluginSetting::Choice{options,..}=declarations.iter().find(|s|matches!(s,PluginSetting::Choice{key:name,..}if name==key)).unwrap()else{panic!()};assert_eq!(options.iter().map(|o|o.value.as_str().unwrap()).collect::<Vec<_>>(),values);assert!(resolve_plugin_settings(&declarations,&json!({key:"invalid"})).is_err());if category=="automation-and-integrations"{assert_eq!(defaults[SENSITIVE_FILTER_SETTING],false);assert!(!declarations.iter().any(|s|matches!(s,PluginSetting::Filter{..})));assert_eq!(resolve_plugin_settings(&settings(category,true),&serde_json::Value::Null).unwrap()[SENSITIVE_FILTER_SETTING],true);}else{assert_eq!(defaults["userIdPrefix"],"");let restored=configured_settings(category,&request(category,"descriptor",json!({"_pluginState":{"config":{"accessFamily":"userAssignments","userIdPrefix":" Ω%_ ' "}}}))).unwrap();let values=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();assert_eq!(values["userIdPrefix"]," Ω%_ ' ");assert_eq!(values["accessFamily"],"userAssignments");assert_eq!(resolve_plugin_settings(&restored,&json!({"userIdPrefix":""})).unwrap()["userIdPrefix"],"");}}}
    #[test]
    fn resource_program_solution_saved_controls_restore_exact_values_and_blank(){
        for (category,key,families,prefix) in [("custom-resources","resourceFamily",vec!["all","themes","translations","images","fileStore"],None),("programs-and-extensions","definitionFamily",vec!["all","services","datasets","extensions","externals"],Some("definitionIdPrefix")),("solutions-and-packages","definitionFamily",vec!["all","solutions","solutionTypes","retainedPackages"],Some("solutionIdPrefix"))]{let declarations=settings(category,false);let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();assert_eq!(defaults[key],"all");assert!(defaults.get("itemsPerPage").is_none());let PluginSetting::Choice{options,..}=declarations.iter().find(|setting|matches!(setting,PluginSetting::Choice{key:name,..}if name==key)).unwrap()else{panic!()};assert_eq!(options.iter().map(|option|option.value.as_str().unwrap()).collect::<Vec<_>>(),families);let mut saved=json!({});saved[key]=json!(families[1]);if let Some(prefix)=prefix{assert_eq!(defaults[prefix],"");saved[prefix]=json!(" Ω%_ ' ");}else{assert!(!declarations.iter().any(|s|matches!(s,PluginSetting::Filter{..})));}let restored=configured_settings(category,&request(category,"descriptor",json!({"_pluginState":{"config":saved}}))).unwrap();let values=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();assert_eq!(values[key],families[1]);if let Some(prefix)=prefix{assert_eq!(values[prefix]," Ω%_ ' ");let mut clear=json!({});clear[prefix]=json!("");assert_eq!(resolve_plugin_settings(&restored,&clear).unwrap()[prefix],"");}}
    }
    #[test]
    fn configurators_business_rules_and_mappings_family_and_id_prefix_restore_literals_blank_and_independent_source_contracts() {
        for (category,families,independent) in [("configurators",vec!["all","configurators","lookups","persistedSources"],"persisted"),("business-rules-and-mappings",vec!["all","forms","structuredOutput","postingRules"],"posting")] {
            let declarations=settings(category,false);let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();assert_eq!(defaults["itemsPerPage"],100);assert_eq!(defaults["definitionFamily"],"all");assert_eq!(defaults["definitionIdPrefix"],"");let PluginSetting::Choice {options,..}=declarations.iter().find(|setting|matches!(setting,PluginSetting::Choice {key,..} if key=="definitionFamily")).unwrap()else{panic!()};assert_eq!(options.iter().map(|option|option.value.as_str().unwrap()).collect::<Vec<_>>(),families);assert!(declarations.iter().any(|setting|matches!(setting,PluginSetting::Filter {description,..} if description.to_lowercase().contains(independent)&&description.contains("Matching contract:"))));
            let restored=configured_settings(category,&request(category,"descriptor",json!({"_pluginState":{"config":{"definitionFamily":families[1],"definitionIdPrefix":" Ω%_ ' "}}}))).unwrap();let values=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();assert_eq!(values["definitionFamily"],families[1]);assert_eq!(values["definitionIdPrefix"]," Ω%_ ' ");assert_eq!(resolve_plugin_settings(&restored,&json!({"definitionIdPrefix":""})).unwrap()["definitionIdPrefix"],"");assert!(resolve_plugin_settings(&declarations,&json!({"definitionFamily":"unknown"})).is_err());
        }
    }

    #[test]
    fn report_selection_declarations_preserve_families_independent_prefixes_and_saved_literals() {
        for (category,page,keys) in [("report-definitions",50,vec![REPORT_DEFINITION_PREFIX_SETTING]),("report-styles-and-layouts",100,vec![REPORT_ID_PREFIX_SETTING,PACKAGE_PATH_PREFIX_SETTING]),("report-behavior",100,vec![REPORT_ID_PREFIX_SETTING])] {
            let declarations=settings(category,false);let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();assert_eq!(defaults["itemsPerPage"],page);
            let mut saved=json!({});for key in &keys {assert_eq!(defaults[*key],"");saved[*key]=json!(" Ω%_ ' ");assert!(declarations.iter().any(|setting|matches!(setting,PluginSetting::Filter {key:name,description,..} if name==key && description.contains("Matching contract:"))));}
            if category=="report-definitions" {assert_eq!(defaults["definitionFamily"],"reportDataAndBaqReports");let PluginSetting::Choice {options,..}=declarations.iter().find(|setting|matches!(setting,PluginSetting::Choice {key,..} if key=="definitionFamily")).unwrap()else{panic!()};assert_eq!(options.iter().map(|option|option.value.as_str().unwrap()).collect::<Vec<_>>(),vec!["reportData","baqReports","reportDataAndBaqReports"]);saved["definitionFamily"]=json!("baqReports");}
            let restored=configured_settings(category,&request(category,"descriptor",json!({"_pluginState":{"config":saved}}))).unwrap();let values=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();
            for key in &keys {assert_eq!(values[*key]," Ω%_ ' ");let mut blank=json!({});blank[*key]=json!("");let cleared=resolve_plugin_settings(&restored,&blank).unwrap();assert_eq!(cleared[*key],"");for other in &keys {if other!=key {assert_eq!(cleared[*other]," Ω%_ ' ");}}}
            if category=="report-definitions" {assert_eq!(values["definitionFamily"],"baqReports");}
        }
    }

    #[test]
    fn menus_and_launch_definitions_menu_prefix_preserves_saved_literals_blank_and_existing_page_default() {
        let declarations=settings("menus-and-launch-definitions",false);let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();assert_eq!(defaults[MENU_PREFIX_SETTING],"");assert_eq!(defaults["itemsPerPage"],4000);
        assert!(declarations.iter().any(|setting|matches!(setting,PluginSetting::Filter {key,label,description,..} if key==MENU_PREFIX_SETTING && label=="Menu ID starts with" && description.contains("menus-and-launch-definitions-menu-id-prefix-v1") && description.contains("custom ancestors"))));
        let restored=configured_settings("menus-and-launch-definitions",&request("menus-and-launch-definitions","descriptor",json!({"_pluginState":{"config":{"menuIdPrefix":" Ω%_ ' ","itemsPerPage":1000}}}))).unwrap();let values=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();assert_eq!(values[MENU_PREFIX_SETTING]," Ω%_ ' ");assert_eq!(values["itemsPerPage"],1000);assert_eq!(resolve_plugin_settings(&restored,&json!({"menuIdPrefix":""})).unwrap()[MENU_PREFIX_SETTING],"");
    }

    #[test]
    fn searches_and_navigation_family_and_search_prefix_preserve_saved_literals_and_page_default() {
        let declarations=settings("searches-and-navigation",false);let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();
        assert_eq!(defaults[SEARCH_FAMILY_SETTING],"all");assert_eq!(defaults[SEARCH_PREFIX_SETTING],"");assert_eq!(defaults["itemsPerPage"],1000);
        let PluginSetting::Choice {options,description,..}=declarations.iter().find(|setting|matches!(setting,PluginSetting::Choice {key,..} if key==SEARCH_FAMILY_SETTING)).unwrap()else{panic!()};
        assert_eq!(options.iter().map(|option|option.value.as_str().unwrap()).collect::<Vec<_>>(),vec!["all","quickSearch","predictiveSearch","namedSearch","enterpriseSearch","savedContextMenu","legacyContextMenu"]);assert!(description.contains("searches-and-navigation-definition-family-v1"));
        assert!(declarations.iter().any(|setting|matches!(setting,PluginSetting::Filter {key,label,description,..} if key==SEARCH_PREFIX_SETTING && label=="Search ID starts with" && description.contains("searches-and-navigation-search-id-prefix-v1") && description.contains("context menus are independent"))));
        let restored=configured_settings("searches-and-navigation",&request("searches-and-navigation","descriptor",json!({"_pluginState":{"config":{"definitionFamily":"legacyContextMenu","searchIdPrefix":" Ω%_ ' ","itemsPerPage":1000}}}))).unwrap();
        let values=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();assert_eq!(values[SEARCH_FAMILY_SETTING],"legacyContextMenu");assert_eq!(values[SEARCH_PREFIX_SETTING]," Ω%_ ' ");
        assert_eq!(resolve_plugin_settings(&restored,&json!({"searchIdPrefix":""})).unwrap()[SEARCH_PREFIX_SETTING],"");assert_eq!(resolve_plugin_settings(&restored,&json!({"definitionFamily":"quickSearch"})).unwrap()[SEARCH_PREFIX_SETTING]," Ω%_ ' ");
        assert!(resolve_plugin_settings(&declarations,&json!({"definitionFamily":"searches"})).is_err());
    }

    #[test]
    fn kinetic_prefix_declarations_preserve_saved_literals_and_existing_options() {
        for (category,key,label,contract) in [("kinetic-applications-and-layers",APPLICATION_PREFIX_SETTING,"Application ID starts with","kinetic-applications-and-layers-application-id-prefix-v1"),("kinetic-personalizations",USER_PREFIX_SETTING,"User ID starts with","kinetic-personalizations-user-id-prefix-v1")] {
            let declarations=settings(category,false);
            let defaults=resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();
            assert_eq!(defaults[key],"");assert_eq!(defaults["itemsPerPage"],100);
            assert!(declarations.iter().any(|setting|matches!(setting,PluginSetting::Filter{key:name,label:caption,description,..} if name==key && caption==label && description.contains(contract))));
            let mut saved=json!({});saved[key]=json!(" Ω%_ ' ");
            if category=="kinetic-applications-and-layers" {assert_eq!(defaults["includeVersionHistory"],true);assert_eq!(defaults["metadataRowsPerPage"],1000);saved["includeVersionHistory"]=json!(false);} else {assert_eq!(defaults["combinedRead"],false);saved["combinedRead"]=json!(true);}
            let restored=configured_settings(category,&request(category,"descriptor",json!({"_pluginState":{"config":saved}}))).unwrap();
            let effective=resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();
            assert_eq!(effective[key]," Ω%_ ' ");let mut blank=json!({});blank[key]=json!("");assert_eq!(resolve_plugin_settings(&restored,&blank).unwrap()[key],"");
            if category=="kinetic-applications-and-layers" {assert_eq!(effective["includeVersionHistory"],false);assert_eq!(effective["metadataRowsPerPage"],1000);} else {assert_eq!(effective["combinedRead"],true);}
        }
    }

    fn request(category: &str, operation: &str, state: serde_json::Value) -> GuestRequest {
        GuestRequest { abi: PLUGIN_ABI, operation: operation.into(), category: category.into(),
            company: "C".into(), identity: None, cursor: None, page_size: None,
            state, response: None, error: None }
    }

    #[test]
    fn classic_prefix_declarations_keep_literal_saved_values_and_existing_options() {
        for (category,keys) in [("classic-customizations",vec![LAYER_PREFIX_SETTING,FORM_PREFIX_SETTING]),("classic-personalizations",vec![USER_PREFIX_SETTING,FORM_PREFIX_SETTING]),("classic-dashboards",vec![DASHBOARD_PREFIX_SETTING])] {
            let declarations = settings(category,false);
            let defaults = resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();
            let filters: Vec<_> = declarations.iter().filter_map(|setting| if let PluginSetting::Filter {key,default,description,..} = setting {Some((key,default,description))} else {None}).collect();
            assert_eq!(filters.len(),keys.len());
            let mut saved = json!({});
            for key in &keys {
                assert_eq!(defaults[*key],"");saved[*key] = json!(" Ω%_ ' ");
                assert!(filters.iter().any(|(name,default,description)|name.as_str() == *key && default.is_empty() && description.contains("Matching contract:")));
            }
            if category == "classic-dashboards" {saved["includeReferencedBAQs"] = json!(false);}
            let restored = configured_settings(category,&request(category,"descriptor",json!({"_pluginState":{"config":saved}}))).unwrap();
            let effective = resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();
            for key in keys {assert_eq!(effective[key]," Ω%_ ' ");let mut blank = json!({});blank[key] = json!("");assert_eq!(resolve_plugin_settings(&restored,&blank).unwrap()[key],"");}
            assert_eq!(effective["itemsPerPage"],if category == "classic-dashboards" {16}else{100});
            if category == "classic-dashboards" {assert_eq!(effective["includeReferencedBAQs"],false);}
        }
    }

    #[test]
    fn bpm_directives_selection_declarations_keep_saved_family_prefix_and_compiled_normalization() {
        let declarations = settings("bpm-directives",false);
        let defaults = resolve_plugin_settings(&declarations,&serde_json::Value::Null).unwrap();
        assert_eq!(defaults[DIRECTIVE_FAMILY_SETTING],"methodAndData"); assert_eq!(defaults[METHOD_PREFIX_SETTING],"");
        let choice = declarations.iter().find(|setting| matches!(setting,PluginSetting::Choice {key,..} if key == DIRECTIVE_FAMILY_SETTING)).unwrap();
        let PluginSetting::Choice {options,description,..} = choice else {panic!()};
        assert_eq!(options.iter().map(|o|(o.label.as_str(),o.value.as_str().unwrap())).collect::<Vec<_>>(),
            vec![("Method and data directives","methodAndData"),("Method directives","method"),("Data directives","data")]);
        assert!(description.contains("bpm-directives-directive-family-v1"));
        assert!(declarations.iter().any(|setting| matches!(setting,PluginSetting::Filter {key,description,..} if key == METHOD_PREFIX_SETTING && description.contains("bpm-directives-method-code-prefix-v1"))));
        assert_eq!(declarations.iter().filter(|setting| matches!(setting,PluginSetting::Checkbox {..})).count(),if cfg!(feature="ordinary-bpm-compiled") {2}else{0});
        let mut saved = json!({"directiveFamily":"data","methodCodePrefix":" Ω%_ ' "});
        if cfg!(feature="ordinary-bpm-compiled") {saved["downloadCompiledAssemblies"]=json!(false);saved["decompileCompiledAssemblies"]=json!(true);}
        let restored = configured_settings("bpm-directives",&request("bpm-directives","descriptor",json!({"_pluginState":{"config":saved}}))).unwrap();
        let resolved = resolve_plugin_settings(&restored,&serde_json::Value::Null).unwrap();
        assert_eq!(resolved[DIRECTIVE_FAMILY_SETTING],"data");assert_eq!(resolved[METHOD_PREFIX_SETTING]," Ω%_ ' ");
        assert_eq!(resolve_plugin_settings(&restored,&json!({"methodCodePrefix":""})).unwrap()[METHOD_PREFIX_SETTING],"");
        assert_eq!(resolve_plugin_settings(&restored,&json!({"directiveFamily":"method"})).unwrap()[METHOD_PREFIX_SETTING]," Ω%_ ' ");
        if cfg!(feature="ordinary-bpm-compiled") {assert_eq!(resolved["downloadCompiledAssemblies"],true);assert_eq!(resolved["decompileCompiledAssemblies"],true);}
    }

    #[test]
    fn group3_declarations_preserve_family_scope_saved_prefix_and_explicit_blank() {
        let declarations = settings("ud-schema", false);
        let default = resolve_plugin_settings(&declarations, &serde_json::Value::Null).unwrap();
        assert_eq!(default[SCHEMA_FAMILY_SETTING], "schemaAndAllMappings");
        assert_eq!(default[SCHEMA_PREFIX_SETTING], ""); assert_eq!(default["itemsPerPage"], 500);
        let family = declarations.iter().find(|setting| matches!(setting, PluginSetting::Choice {key,..} if key == SCHEMA_FAMILY_SETTING)).unwrap();
        let PluginSetting::Choice { options, description, .. } = family else {panic!()};
        assert_eq!(options.iter().map(|option| (option.label.as_str(), option.value.as_str().unwrap())).collect::<Vec<_>>(),
            vec![("Schema tables", "schema"), ("UD mappings", "mappings"), ("Schema tables and all UD mappings", "schemaAndAllMappings")]);
        assert!(description.contains("ud-schema-definition-family-v1"));
        assert!(resolve_plugin_settings(&declarations, &json!({"definitionFamily":"all"})).is_err());
        for (category, key, version) in [("ud-schema", SCHEMA_PREFIX_SETTING, "ud-schema-schema-table-id-prefix-v1"), ("extended-properties", PROPERTY_PREFIX_SETTING, "extended-properties-table-id-prefix-v1")] {
            let mut config = json!({}); config[key] = json!(" Ω%_ ' ");
            if category == "ud-schema" {config[SCHEMA_FAMILY_SETTING] = json!("mappings");}
            let declarations = configured_settings(category, &request(category, "descriptor", json!({"_pluginState":{"config":config}}))).unwrap();
            assert!(declarations.iter().any(|setting| matches!(setting, PluginSetting::Filter {key:filter_key,description,..} if filter_key == key && description.contains(version))));
            let saved = resolve_plugin_settings(&declarations, &serde_json::Value::Null).unwrap(); assert_eq!(saved[key], " Ω%_ ' ");
            let mut blank = json!({}); blank[key] = json!("");
            assert_eq!(resolve_plugin_settings(&declarations, &blank).unwrap()[key], "");
            if category == "ud-schema" {
                let schema = resolve_plugin_settings(&declarations, &json!({"definitionFamily":"schema"})).unwrap();
                assert_eq!(schema[key], " Ω%_ ' "); assert_eq!(schema[SCHEMA_FAMILY_SETTING], "schema");
            }
        }
    }

    #[test]
    fn group2_prefix_declarations_preserve_variant_options_and_literal_defaults() {
        for (category, key, label, version) in [
            ("baqs-and-ubaqs", QUERY_PREFIX_SETTING, "Query ID starts with", "baqs-and-ubaqs-query-id-prefix-v1"),
            ("functions", LIBRARY_PREFIX_SETTING, "Library ID starts with", "functions-library-id-prefix-v1"),
        ] {
            let declarations = settings(category, false);
            let filters: Vec<_> = declarations.iter().filter_map(|setting| match setting {
                PluginSetting::Filter { key, label, default, description, .. } => Some((key, label, default, description)),
                _ => None,
            }).collect();
            assert_eq!(filters.len(), 1);
            assert_eq!(filters[0].0, key);
            assert_eq!(filters[0].1, label);
            assert_eq!(filters[0].2, "");
            assert!(filters[0].3.contains(version));
            let compiled = if category == "baqs-and-ubaqs" { cfg!(feature="ubaq-compiled") } else { cfg!(feature="function-compiled") };
            assert_eq!(declarations.iter().filter(|setting| matches!(setting, PluginSetting::Checkbox { .. })).count(), if compiled { 2 } else { 0 });
            let mut saved_values = json!({});
            saved_values[key] = json!(" Ω%_ ' ");
            let saved = json!({"_pluginState":{"config":saved_values}});
            let declarations = configured_settings(category, &request(category, "descriptor", saved)).unwrap();
            assert_eq!(resolve_plugin_settings(&declarations, &serde_json::Value::Null).unwrap()[key], " Ω%_ ' ");
            let mut blank = json!({});
            blank[key] = json!("");
            assert_eq!(resolve_plugin_settings(&declarations, &blank).unwrap()[key], "");
        }
    }

    #[test]
    fn saved_options_survive_a_new_descriptor_and_selection_only_update() {
        for category in ["ud-codes", "ud-schema", "extended-properties", "baqs-and-ubaqs", "bpm-directives", "functions", "classic-customizations", "classic-personalizations", "classic-dashboards", "kinetic-applications-and-layers", "kinetic-personalizations", "automation-and-integrations"] {
            let declarations = settings(category, false);
            let mut expected = resolve_plugin_settings(&declarations, &serde_json::Value::Null).unwrap();
            for declaration in declarations {
                match declaration {
                    PluginSetting::Integer { key, .. } => expected[key] = json!(1234),
                    PluginSetting::Checkbox { key, default, .. } => expected[key] = json!(!default),
                    PluginSetting::Filter { key, .. } => expected[key] = json!(" Prefix Ω%_ ' "),
                    PluginSetting::Choice { key, options, .. } => expected[key] = options[0].value.clone(),
                }
            }
            let GuestReply::State { plugin_state, .. } = crate::handle(category,
                request(category, "configure", json!({"values":expected,"selected":true}))) else { panic!() };
            // A fresh descriptor is the route used by a restarted GUI or CLI.
            let GuestReply::Descriptor { category: descriptor, .. } = crate::handle(category,
                request(category, "descriptor", json!({"_pluginState":plugin_state}))) else { panic!() };
            assert_eq!(resolve_plugin_settings(&descriptor.settings, &serde_json::Value::Null).unwrap(), expected, "{category}");
            let GuestReply::State { plugin_state, .. } = crate::handle(category,
                request(category, "configure", json!({"_pluginState":plugin_state,"values":{},"selected":false}))) else { panic!() };
            assert_eq!(plugin_state["config"], expected, "{category}");
            assert_eq!(plugin_state["selected"], false);
        }
    }

    #[test]
    fn restored_page_size_reaches_the_native_query() {
        let descriptor = configured_settings("kinetic-personalizations", &request("kinetic-personalizations", "descriptor",
            json!({"_pluginState":{"config":{"itemsPerPage":1000}}}))).unwrap();
        let values = resolve_plugin_settings(&descriptor, &serde_json::Value::Null).unwrap();
        // Discovery first freezes the native recovery format (GetBaseLevel).
        let mut discover = request("kinetic-personalizations", "discover",
            json!({"scope":{"tenant":""},"_pluginSettings":values}));
        let GuestReply::Action { action: herbert_contract::HostAction::SharedNativeRead { method, .. }, state } =
            crate::handle("kinetic-personalizations", discover.clone()) else { panic!("recovery format read expected") };
        assert_eq!(method, "GetBaseLevel");
        discover.state = state;
        discover.response = Some(json!({"returnObj":{"_Major":4,"_Minor":3,"_Build":200,"_Revision":39}}));
        let reply = crate::handle("kinetic-personalizations", discover);
        let GuestReply::Action { action: herbert_contract::HostAction::Query { query }, .. } = reply else { panic!("{reply:?}") };
        assert_eq!(query.page_size, Some(1000));
    }
}
use std::collections::BTreeSet;

pub fn for_methods(methods: &[(String, String)]) -> Vec<NativeResponsePolicy> {
    methods.iter().map(|(service, _)| service).collect::<BTreeSet<_>>().into_iter()
        .filter(|service| service.contains(".BO.") || (service.as_str() == "Ice.Lib.MetaFXSvc"
            && methods.iter().any(|(declared_service, method)| declared_service == *service && method == "GetApplications")))
        .map(|service| {
            if service == "Ice.Lib.MetaFXSvc" {
                // Catalog rows remain available in memory for ownership checks.
                // The saved copy excludes System rows but keeps custom layers
                // beneath an excluded System application.
                return NativeResponsePolicy {
                    service: service.clone(),
                    retention_rules: vec![
                        NativeRetentionRule {
                            methods: vec!["GetApplications".into()],
                            array_path: ["returnObj", "*", "Layers"].map(String::from).to_vec(),
                            exclude_when: NativeRowPredicate::FlagTrue { field: "SystemFlag".into() },
                            preserve_children: vec![],
                        },
                        NativeRetentionRule {
                            methods: vec!["GetApplications".into()],
                            array_path: vec!["returnObj".into()],
                            exclude_when: NativeRowPredicate::FlagTrue { field: "SystemFlag".into() },
                            preserve_children: vec!["Layers".into()],
                        },
                    ],
                    ..Default::default()
                };
            }
            let mut policy = NativeResponsePolicy {
                service: service.clone(),
                tables: Some(NativeTablePolicy {
                    pointer: "/returnObj".into(), fallback_to_root: true,
                    company_field: "Company".into(), global_values: vec![String::new()],
                    // Shared definitions include assignments for other companies.
                    unscoped_tables: ["ReportComp", "ExtensionSetMapping", "EfxLibraryMapping"].map(String::from).to_vec(),
                    // Configurator audit history is not an authored definition.
                    omitted_tables: vec!["PcAudit".into()],
                }),
                ..Default::default()
            };
            if service == "Ice.BO.ZDataTableSvc" {
                policy.retention_rules.push(NativeRetentionRule {
                    methods: vec!["GetRows".into()],
                    array_path: ["returnObj", "ZDataTable"].map(String::from).to_vec(),
                    exclude_when: NativeRowPredicate::FlagTrue { field: "SystemFlag".into() },
                    preserve_children: vec![],
                });
            }
            // This exact native envelope was observed in ownership inventories.
            // Keep original rows in memory for the query ownership checks.
            // HostAction::Query is not a NativeRead policy route; SolutionType
            // excludes System rows in its own source QuerySpec instead.
            let retained_table = match service.as_str() {
                "Ice.BO.BAQDesignerSvc" => Some(("GetList", "DynamicQueryDesignerList")),
                _ => None,
            };
            if let Some((method, table)) = retained_table {
                if methods.iter().any(|(declared_service, declared_method)| declared_service == service && declared_method == method) {
                    policy.retention_rules.push(NativeRetentionRule {
                        methods: vec![method.into()],
                        array_path: vec!["returnObj".into(), table.into()],
                        exclude_when: NativeRowPredicate::FlagTrue { field: "SystemFlag".into() },
                        preserve_children: vec![],
                    });
                }
            }
            if matches!(service.as_str(), "Ice.BO.SysAgentSvc" | "Ice.BO.ProcessSetSvc" | "Erp.BO.GlbAlertSvc") {
                policy.sensitive_filter_setting = Some(SENSITIVE_FILTER_SETTING.into());
                policy.redact_fields = ["SysPassWord", "SysPassWordMasked", "Password", "ApiKey", "ClientSecret", "Secret", "ParamToken"].map(String::from).to_vec();
                policy.redact_parameters = vec![ParameterRedaction {
                    name_field: "ParamName".into(),
                    name_contains: ["password", "secret", "apikey", "access_token"].map(String::from).to_vec(),
                    value_fields: ["ParamCharacter", "ParamToken"].map(String::from).to_vec(),
                }];
            }
            policy
        }).collect()
}

pub fn business_graph_policies(methods: &[(String, String)]) -> Vec<NativeResponsePolicy> {
    let mut policies = for_methods(methods);
    for policy in &mut policies {
        if policy.service == "Ice.BO.ZDataTableSvc" {
            // The Business rules and mappings plugin reads system table headers as decoding schema, not authored
            // schema artifacts. Recovery needs those headers with their fields.
            // The UD schema plugin's authored-schema exclusion remains in for_methods.
            policy.retention_rules.clear();
        }
    }
    policies
}

pub fn kinetic_page_policies(methods: &[(String, String)]) -> Vec<NativeResponsePolicy> {
    let mut policies = for_methods(methods);
    for policy in &mut policies {
        if policy.service == "Ice.BO.GenXDataSvc" {
            // The Kinetic applications and layers plugin's exact parent read explicitly includes the authenticated
            // tenant company as well as selected/global scope. The generic BO
            // company-only filter would erase valid tenant parents before the
            // collector can validate them. kinetic_pages validates every native
            // row against its session-derived scopes, type and requested keys.
            // This exception belongs only to Kinetic applications and layers; other GenXData consumers keep
            // their existing response policy. No host ABI extension is needed.
            policy.tables = None;
        }
    }
    policies
}

#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::{GuestReply, Identity, Scope, PLUGIN_ABI};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn kinetic_descriptor_retains_session_scoped_genx_parents_without_changing_generic_policy() {
        let mut request = catalog_request();
        request.operation = "descriptor".into();
        let GuestReply::Descriptor { methods, response_policies, .. } = crate::handle("kinetic-applications-and-layers", request) else { panic!() };
        assert!(methods.iter().any(|(service, method)| service == "Ice.BO.GenXDataSvc" && method == "GetRows"));
        let native = response_policies.iter().find(|policy| policy.service == "Ice.BO.GenXDataSvc").unwrap();
        assert!(native.tables.is_none(), "Tenant parent rows must reach the Kinetic applications and layers scope validator intact");
        assert!(native.retention_rules.is_empty());
        assert!(native.redact_fields.is_empty());
        assert!(native.redact_parameters.is_empty());

        let declared = crate::kinetic_pages::methods();
        let generic = for_methods(&declared);
        let generic_genx = generic.iter().find(|policy| policy.service == "Ice.BO.GenXDataSvc").unwrap();
        let tables = generic_genx.tables.as_ref().expect("Other collectors retain company filtering");
        assert_eq!(tables.company_field, "Company");
        assert_eq!(tables.global_values, vec![String::new()]);
        // The routing change must affect only this one Kinetic applications and layers policy field.
        let mut expected = generic;
        expected.iter_mut().find(|policy| policy.service == "Ice.BO.GenXDataSvc").unwrap().tables = None;
        assert_eq!(serde_json::to_value(response_policies).unwrap(), serde_json::to_value(expected).unwrap());
    }

    #[test]
    fn posting_graph_retains_decoding_schema_without_changing_authored_schema_policy() {
        let methods = crate::business_pages::methods();
        let graph = business_graph_policies(&methods);
        let schema = graph.iter().find(|p| p.service == "Ice.BO.ZDataTableSvc").unwrap();
        assert!(schema.retention_rules.is_empty());
        assert!(schema.tables.is_some());
        let authored = for_methods(&crate::schema_pages::methods());
        let schema = authored.iter().find(|p| p.service == "Ice.BO.ZDataTableSvc").unwrap();
        assert_eq!(schema.retention_rules.len(), 1);
        assert_eq!(schema.retention_rules[0].array_path, vec!["returnObj", "ZDataTable"]);
    }

    #[test]
    fn kinetic_retention_is_method_scoped_children_first_and_not_sensitive_redaction() {
        let policies = for_methods(&crate::kinetic::methods());
        assert_eq!(policies.len(), 1);
        let policy = &policies[0];
        assert_eq!(policy.service, "Ice.Lib.MetaFXSvc");
        assert!(policy.tables.is_none());
        assert!(policy.sensitive_filter_setting.is_none());
        assert!(policy.redact_fields.is_empty());
        assert!(policy.redact_parameters.is_empty());
        assert_eq!(serde_json::to_value(&policy.retention_rules).unwrap(), json!([
            {"methods":["GetApplications"],"arrayPath":["returnObj","*","Layers"],
             "excludeWhen":{"type":"flagTrue","field":"SystemFlag"},"preserveChildren":[]},
            {"methods":["GetApplications"],"arrayPath":["returnObj"],
             "excludeWhen":{"type":"flagTrue","field":"SystemFlag"},"preserveChildren":["Layers"]}
        ]));
        // Other MetaFX calls must not acquire catalog-shaped projection rules.
        assert!(for_methods(&[("Ice.Lib.MetaFXSvc".into(), "GetLayer".into())]).is_empty());
        let unrelated = for_methods(&[("Ice.BO.SysAgentSvc".into(), "GetRows".into())]);
        assert!(unrelated[0].retention_rules.is_empty());
    }

    fn catalog_request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI, operation: "discover".into(), category: "kinetic-applications-and-layers".into(),
            company: "EXAMPLE".into(), identity: None, cursor: None, page_size: Some(100),
            state: json!({}), response: None, error: None,
        }
    }

    #[test]
    fn query_metadata_retention_uses_verified_wire_tables_and_exact_methods() {
        for (service, method, table) in [
            ("Ice.BO.BAQDesignerSvc", "GetList", "DynamicQueryDesignerList"),
        ] {
            let policies = for_methods(&[(service.into(), method.into())]);
            assert_eq!(policies.len(), 1);
            assert_eq!(serde_json::to_value(&policies[0].retention_rules).unwrap(), json!([
                {"methods":[method],"arrayPath":["returnObj",table],
                 "excludeWhen":{"type":"flagTrue","field":"SystemFlag"},"preserveChildren":[]}
            ]));
            assert!(policies[0].sensitive_filter_setting.is_none());
            assert!(policies[0].redact_fields.is_empty());
            assert!(policies[0].redact_parameters.is_empty());
            let unrelated = for_methods(&[(service.into(), "GetByID".into())]);
            assert!(unrelated[0].retention_rules.is_empty());
        }
        let ubaq = for_methods(&crate::collectors::methods("ubaqs"));
        let owner = ubaq.iter().find(|p| p.service == "Ice.BO.BAQDesignerSvc").unwrap();
        assert_eq!(owner.retention_rules.len(), 1);
    }

    #[test]
    fn mixed_catalog_keeps_custom_children_and_optional_layers_without_system_targets() {
        let mut request = catalog_request();
        request.response = Some(json!({"returnObj":[
            {"Id":"Stock","SystemFlag":true,"Layers":[
                {"Company":"EXAMPLE","TypeCode":"KNTCCustLayer","LayerName":"custom","DeviceType":"Desktop","SystemFlag":false},
                {"Company":"EXAMPLE","TypeCode":"KNTCCustLayer","LayerName":"stock","DeviceType":"Desktop","SystemFlag":true}
            ]},
            {"Id":"CustomWithoutLayers","Type":"view","SubType":"Dashboard","SystemFlag":false,"SecurityCode":"unchanged","CreatedBy":"creator"},
            {"Id":"CustomNullLayers","Type":"view","SubType":"Dashboard","SystemFlag":false,"Layers":null},
            {"Id":"StockWithoutLayers","SystemFlag":true},
            {"Id":"StockNullLayers","SystemFlag":true,"Layers":null}
        ]}));
        let GuestReply::Inventory { identities, authoritative, diagnostics, .. } = crate::kinetic::handle(request) else { panic!("Expected native catalog inventory") };
        assert!(authoritative);
        assert!(diagnostics.is_empty());
        assert_eq!(identities.len(), 3);
        assert_eq!(identities.iter().filter(|id| id.kind == "MetaFXApplication").count(), 2);
        let layer = identities.iter().find(|id| id.kind == "MetaFXLayer").unwrap();
        assert_eq!(layer.keys["ViewId"], "Stock");
        assert_eq!(layer.keys["LayerName"], "custom");
        assert!(!identities.iter().any(|id| id.kind == "MetaFXApplication" && id.keys["ViewId"].starts_with("Stock")));
    }

    #[test]
    fn targeted_system_catalog_keeps_explicit_exclusion_with_original_in_memory_response() {
        let mut request = catalog_request();
        request.operation = "acquire".into();
        request.identity = Some(Identity {
            category: "kinetic-applications-and-layers".into(), kind: "MetaFXApplication".into(),
            scope: Scope::Company { company: "EXAMPLE".into() },
            keys: BTreeMap::from([
                ("ViewId".into(), "Stock".into()), ("Company".into(), "EXAMPLE".into()),
                ("Type".into(), "view".into()), ("SubType".into(), "Apps".into()),
                ("DeviceType".into(), "Desktop".into()),
            ]),
        });
        request.state = json!({"phase":"systemPolicy","kineticReadSchema":3,"_hostCaller":"manager"});
        request.response = Some(json!({"returnObj":[{"Id":"Stock","SystemFlag":true,"Layers":null}]}));
        assert!(matches!(crate::kinetic::handle(request), GuestReply::Failure { diagnostic }
            if diagnostic.code == "SYSTEM_ARTIFACT_EXCLUDED"));
    }

    #[test]
    fn only_declared_bo_services_receive_the_plugins_collection_rules() {
        let policies = for_methods(&[
            ("Ice.BO.SysAgentSvc".into(), "GetByID".into()),
            ("Ice.BO.SysAgentSvc".into(), "GetRows".into()),
            ("Erp.BO.ConfiguratorDefSvc".into(), "GetByID".into()),
            ("Ice.Lib.OtherSvc".into(), "Read".into()),
        ]);
        assert_eq!(policies.len(), 2);
        let automation = policies.iter().find(|p| p.service == "Ice.BO.SysAgentSvc").unwrap();
        assert_eq!(automation.sensitive_filter_setting.as_deref(),Some(SENSITIVE_FILTER_SETTING));
        assert!(automation.redact_fields.contains(&"SysPassWord".into()));
        assert_eq!(automation.redact_parameters[0].name_field,"ParamName");
        let configurator = policies.iter().find(|p| p.service == "Erp.BO.ConfiguratorDefSvc").unwrap();
        assert!(configurator.redact_fields.is_empty());
        assert_eq!(configurator.tables.as_ref().unwrap().omitted_tables,vec!["PcAudit"]);
    }
}
