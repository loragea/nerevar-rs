pub mod commands;
pub mod instance_delete;
pub mod instance_edit;

// No flattening re-exports here on purpose: lib.rs registers these commands by
// their full module path (`connection::instance_edit::update_instance`, ...),
// so re-exporting them at the module root was dead weight.
