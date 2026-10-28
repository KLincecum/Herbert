#![recursion_limit = "256"]
//! Herbert's shared host. The binary selects a frontend over these same operations.
mod about;
mod activity;
mod file_writers;
mod page_buffer;
mod activity_metrics;
mod collection_preferences;
mod mutation_preferences;
mod server_changes;
mod decompiler_settings;
mod progress_snapshot;
mod shared_reads;
mod server_collection;
pub mod access;
mod access_store;
pub mod application;
mod archives;
pub mod baq;
mod checkpoint_log;
pub mod connection;
#[cfg(feature = "gui")]
pub mod gui;
mod help;
mod helper_libraries;
mod helper_recovery;
mod helpers;
pub mod input;
mod native_access;
pub mod native_export;
mod persisted_chunks;
mod output_layout;
mod response_policy;
mod response_retention;
mod plugin_artifacts;
mod plugin_state;
pub mod plugins;
pub mod processing;
pub mod profiles;
pub mod recovery;
pub mod scheduler;
pub mod stdio;
pub mod storage;
mod system_info;
mod xml_archive;

#[cfg(target_os = "windows")]
mod windows_credentials;

mod native_zip;
mod native_artifact_backend;
mod native_artifact_package;
mod native_artifact_store;

mod collection_output;
mod processing_status;

#[cfg(test)]
mod baq_local_native_tests;

#[cfg(test)]
mod configurator_wasm_tests;
#[cfg(test)]
mod business_wasm_tests;

mod support_packet;

mod decompiler;
mod raw_decompiler;
