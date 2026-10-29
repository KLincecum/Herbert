//! Descriptors supplied by this optional collection package. The host has no roster.
pub const CATALOG: &[(&str, &str, &str)] = &[
    (
        "ud-codes",
        "UD codes",
        "Code types and every code entry with native identity and metadata.",
    ),
    (
        "ud-schema",
        "UD schema",
        "Authored UD fields, tables, relationships, indexes and mappings.",
    ),
    (
        "extended-properties",
        "Extended properties",
        "Authored table and field properties and bindings.",
    ),
    (
        "baqs-and-ubaqs",
        "BAQs and UBAQs",
        "Query definitions, diagrams, update settings, widgets, custom code and local native-format packages.",
    ),
    (
        "bpm-directives",
        "BPM directives",
        "Method and data directives, widgets, code and native packages in separate directories.",
    ),
    (
        "functions",
        "Functions",
        "Libraries, functions, signatures, source variants and references.",
    ),
    (
        "classic-customizations",
        "Classic customizations",
        "Full composite identities, variants, code and resources.",
    ),
    (
        "classic-personalizations",
        "Classic personalizations",
        "Persisted all-user definitions with ownership and scope.",
    ),
    (
        "classic-dashboards",
        "Classic dashboards",
        "Authored trackers, views, query links and deployments.",
    ),
    (
        "kinetic-applications-and-layers",
        "Kinetic applications and layers",
        "Authored layouts, events, rules, drafts and revisions.",
    ),
    (
        "kinetic-personalizations",
        "Kinetic personalizations",
        "All-user personalizations and home-page layouts.",
    ),
    (
        "searches-and-navigation",
        "Searches and navigation",
        "Standalone searches, context links and process links.",
    ),
    (
        "menus-and-launch-definitions",
        "Menus and launch definitions",
        "Custom and demonstrably modified entries and ancestors.",
    ),
    (
        "report-definitions",
        "Report definitions",
        "RDD and BAQ report authored definitions.",
    ),
    (
        "report-styles-and-layouts",
        "Report styles and layouts",
        "Native returned layouts, subreports and resources.",
    ),
    (
        "report-behavior",
        "Report behavior",
        "Routing, breaking, delivery and authored print behavior.",
    ),
    (
        "configurators",
        "Configurators",
        "Retained revisions, rules, methods, lookups and assets.",
    ),
    (
        "business-rules-and-mappings",
        "Business rules and mappings",
        "Authored module rules, mappings and posting rules.",
    ),
    (
        "custom-resources",
        "Custom resources",
        "Original images, scripts, themes, translations and assets.",
    ),
    (
        "programs-and-extensions",
        "Programs and extensions",
        "Exposed binaries, symbols, registrations and SDK metadata.",
    ),
    (
        "solutions-and-packages",
        "Solutions and packages",
        "Membership and retained packages without a new build.",
    ),
    (
        "automation-and-integrations",
        "Automation and integrations",
        "Authored schedules, alerts and integration definitions, with optional sensitive-value filtering.",
    ),
    (
        "access-and-assignments",
        "Access and assignments",
        "Relevant security and assignment configuration.",
    ),
    (
        "configuration-references",
        "Configuration references",
        "Preserve table references from collected definitions. Do not download the referenced business rows.",
    ),
];
