//! Descriptors supplied by this optional collection package. The host has no roster.
pub const CATALOG: &[(&str, &str, &str)] = &[
    (
        "C01",
        "UD Codes",
        "Code types and every code entry with native identity and metadata.",
    ),
    (
        "C02",
        "UD schema",
        "Authored UD fields, tables, relationships, indexes and mappings.",
    ),
    (
        "C03",
        "Extended properties",
        "Authored table and field properties and bindings.",
    ),
    (
        "C04",
        "BAQs and UBAQs",
        "Query definitions, diagrams, update settings, widgets, custom code and local native-format packages.",
    ),
    (
        "C06",
        "BPM directives",
        "Method and data directives, widgets, code and native packages in separate directories.",
    ),
    (
        "C08",
        "Functions",
        "Libraries, functions, signatures, source variants and references.",
    ),
    (
        "C09",
        "Classic customizations",
        "Full composite identities, variants, code and resources.",
    ),
    (
        "C10",
        "Classic personalizations",
        "Persisted all-user definitions with ownership and scope.",
    ),
    (
        "C11",
        "Classic dashboards",
        "Authored trackers, views, query links and deployments.",
    ),
    (
        "C12",
        "Kinetic applications and layers",
        "Authored layouts, events, rules, drafts and revisions.",
    ),
    (
        "C13",
        "Kinetic personalizations",
        "All-user personalizations and home-page layouts.",
    ),
    (
        "C14",
        "Searches and navigation",
        "Standalone searches, context links and process links.",
    ),
    (
        "C15",
        "Menus and launch definitions",
        "Custom and demonstrably modified entries and ancestors.",
    ),
    (
        "C16",
        "Report definitions",
        "RDD and BAQ report authored definitions.",
    ),
    (
        "C17",
        "Report styles and layouts",
        "Native returned layouts, subreports and resources.",
    ),
    (
        "C18",
        "Report behavior",
        "Routing, breaking, delivery and authored print behavior.",
    ),
    (
        "C19",
        "Configurators",
        "Retained revisions, rules, methods, lookups and assets.",
    ),
    (
        "C20",
        "Business rules and mappings",
        "Authored module rules, mappings and posting rules.",
    ),
    (
        "C21",
        "Custom resources",
        "Original images, scripts, themes, translations and assets.",
    ),
    (
        "C22",
        "Programs and extensions",
        "Exposed binaries, symbols, registrations and SDK metadata.",
    ),
    (
        "C24",
        "Solutions and packages",
        "Membership and retained packages without a new build.",
    ),
    (
        "C25",
        "Automation and integrations",
        "Authored schedules, alerts and integration definitions, with optional sensitive-value filtering.",
    ),
    (
        "C26",
        "Access and assignments",
        "Relevant security and assignment configuration.",
    ),
    (
        "C27",
        "Configuration references",
        "Preserve table references from collected definitions. Do not download the referenced business rows.",
    ),
];
