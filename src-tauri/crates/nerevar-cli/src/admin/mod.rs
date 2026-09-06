//! `nerevar-cli admin` — the co-admin surface for a headless host.
//!
//! One HTTP client ([`client`]) against a host's `/admin` routes, plus the two
//! things a co-admin does that the routes do not do for them: pick a token out
//! of three possible sources ([`token`]), and edit a load-order document
//! ([`load_order`]) so that `enable`, `disable` and `order` are single
//! commands rather than a fetch-edit-post by hand.
//!
//! Every command takes the same shape: build the request, let the host decide,
//! print either its raw body (`--json`) or a summary ([`render`]). Business
//! logic stays on the host — the CLI never writes to the host's filesystem,
//! never decides what an apply does, and never validates a load order the
//! host is about to validate itself.

pub mod archive;
pub mod client;
pub mod load_order;
pub mod render;
pub mod token;

use std::io::Write;

use nerevar_core::admin::{AdminStatus, StagedPackage};
use nerevar_core::instance_data::LoadOrder;

use crate::cli::{AdminAction, AdminOptions, LoadOrderAction};
use client::AdminClient;

const STATUS_ROUTE: &str = "/admin/status";
const LOAD_ORDER_ROUTE: &str = "/admin/load-order";

/// Builds the client for `options`, failing before any request when the host
/// address is unusable or no token was given.
pub fn connect(options: &AdminOptions) -> Result<AdminClient, String> {
    let host = options
        .host
        .as_deref()
        .ok_or_else(|| "No host. Pass --host <hostname, IP, or http(s):// URL>.".to_string())?;
    let token = token::resolve_token(
        options.token.as_deref(),
        options.token_file.as_deref(),
        token::token_from_environment().as_deref(),
    )?;
    AdminClient::new(host, options.port, token)
}

/// Runs one `admin` subcommand against `client`, writing what a person reads
/// to `out`.
///
/// The client is passed in rather than built here so a test can drive these
/// against an in-process server, and so `--json` and the summary path share
/// exactly one request each.
pub async fn run(
    client: &AdminClient,
    options: &AdminOptions,
    action: &AdminAction,
    out: &mut dyn Write,
) -> Result<(), String> {
    match action {
        AdminAction::Status => {
            let response = client.get(STATUS_ROUTE).await?.into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            let status: AdminStatus = parse(&response.body)?;
            render::status(&status, client.base_url(), out)
        }

        AdminAction::Upload { archive, name } => {
            let name = match name {
                Some(name) => nerevar_core::admin::validate_staged_package_name(name)?,
                None => archive::package_name_from_archive(archive)?,
            };
            let response = client
                .put_file(&format!("/admin/packages/{name}"), archive)
                .await?
                .into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            let staged: StagedPackage = parse(&response.body)?;
            render::uploaded(&staged, out)
        }

        AdminAction::Remove { name } => {
            let response = client
                .delete(&format!("/admin/packages/{name}"))
                .await?
                .into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            writeln!(out, "Marked \"{name}\" for removal at apply.").map_err(write_error)?;
            pending_from(&response.body, out)
        }

        AdminAction::LoadOrder { action } => load_order_command(client, options, action, out).await,

        AdminAction::Enable { name } => {
            edit_load_order(client, options, out, |order, staged| {
                let resolved = load_order::set_enabled(order, staged, name, true)?;
                Ok(format!("Enabled \"{resolved}\" in the staged load order."))
            })
            .await
        }

        AdminAction::Disable { name } => {
            edit_load_order(client, options, out, |order, staged| {
                let resolved = load_order::set_enabled(order, staged, name, false)?;
                Ok(format!("Disabled \"{resolved}\" in the staged load order."))
            })
            .await
        }

        AdminAction::Order { names } => {
            edit_load_order(client, options, out, |order, _| {
                load_order::reorder(order, names)?;
                Ok(format!(
                    "Moved {} to the front of the staged load order.",
                    names.join(", ")
                ))
            })
            .await
        }

        AdminAction::Apply => {
            let response = client.post("/admin/apply").await?.into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            render::applied(&response.json()?, out)
        }

        AdminAction::Discard => {
            let response = client.post("/admin/discard").await?.into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            writeln!(out, "Staging cleared.").map_err(write_error)?;
            pending_from(&response.body, out)
        }

        AdminAction::Restart { yes } => {
            if !yes && !confirm_restart(client.base_url())? {
                writeln!(out, "Cancelled; nothing was restarted.").map_err(write_error)?;
                return Ok(());
            }
            let response = client.post("/admin/restart").await?.into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            render::restarted(&response.json()?, out)
        }
    }
}

async fn load_order_command(
    client: &AdminClient,
    options: &AdminOptions,
    action: &LoadOrderAction,
    out: &mut dyn Write,
) -> Result<(), String> {
    match action {
        LoadOrderAction::Get { pending } => {
            let response = client.get(LOAD_ORDER_ROUTE).await?.into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            let view = response.json()?;
            let document = if *pending {
                match view.get("pending") {
                    Some(serde_json::Value::Null) | None => {
                        return Err(
                            "No load order is staged; `load-order get` shows the one on disk."
                                .to_string(),
                        )
                    }
                    Some(document) => document,
                }
            } else {
                view.get("current").ok_or_else(|| {
                    "The host's load-order reply had no \"current\" document.".to_string()
                })?
            };
            raw(
                &serde_json::to_string_pretty(document).map_err(|e| e.to_string())?,
                out,
            )
        }

        LoadOrderAction::Set { file } => {
            let text = std::fs::read_to_string(file)
                .map_err(|error| format!("Failed to read {}: {error}", file.display()))?;
            // Parsed here only to fail on a typo before the upload; the host
            // validates it properly and its verdict is the one that counts.
            let document: LoadOrder = serde_json::from_str(&text).map_err(|error| {
                format!("{} is not a load-order document: {error}", file.display())
            })?;
            let response = client
                .post_json(
                    LOAD_ORDER_ROUTE,
                    &serde_json::to_value(&document).map_err(|e| e.to_string())?,
                )
                .await?
                .into_success()?;
            if options.json {
                return raw(&response.body, out);
            }
            writeln!(
                out,
                "Staged the load order from {} ({} entries).",
                file.display(),
                document.entries.len()
            )
            .map_err(write_error)?;
            pending_from(&response.body, out)
        }
    }
}

/// The shared body of `enable`, `disable` and `order`: fetch the document the
/// next apply will start from, let `edit` change it, post it back.
///
/// The pending document wins over the one on disk, so two edits in a row
/// compose instead of the second undoing the first.
async fn edit_load_order<F>(
    client: &AdminClient,
    options: &AdminOptions,
    out: &mut dyn Write,
    edit: F,
) -> Result<(), String>
where
    F: FnOnce(&mut LoadOrder, &[StagedPackage]) -> Result<String, String>,
{
    let view = client.get(LOAD_ORDER_ROUTE).await?.into_success()?.json()?;
    let source = match view.get("pending") {
        Some(serde_json::Value::Null) | None => view.get("current"),
        pending => pending,
    }
    .ok_or_else(|| "The host's load-order reply had no document in it.".to_string())?;
    let mut document: LoadOrder = serde_json::from_value(source.clone())
        .map_err(|error| format!("The host's load order did not parse: {error}"))?;

    // Only needed to enable a package that was uploaded but not yet applied,
    // so it costs one extra request on exactly the commands that might need
    // it and none on the rest.
    let staged = staged_packages(client).await?;

    let note = edit(&mut document, &staged)?;

    let response = client
        .post_json(
            LOAD_ORDER_ROUTE,
            &serde_json::to_value(&document).map_err(|e| e.to_string())?,
        )
        .await?
        .into_success()?;

    if options.json {
        return raw(&response.body, out);
    }
    writeln!(out, "{note}").map_err(write_error)?;
    writeln!(out, "Staged, not live: run `nerevar-cli admin apply`.").map_err(write_error)?;
    pending_from(&response.body, out)
}

/// The packages uploaded but not yet applied, from `GET /admin/status`.
async fn staged_packages(client: &AdminClient) -> Result<Vec<StagedPackage>, String> {
    let status: AdminStatus = parse(&client.get(STATUS_ROUTE).await?.into_success()?.body)?;
    Ok(status
        .pending_changes
        .map(|changes| changes.staged)
        .unwrap_or_default())
}

/// Renders the `pendingChanges` every staging write answers with.
fn pending_from(body: &str, out: &mut dyn Write) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Body {
        pending_changes: Option<nerevar_core::admin::AdminPendingChanges>,
    }
    let parsed: Body = parse(body)?;
    render::pending(parsed.pending_changes.as_ref(), out)
}

fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, String> {
    serde_json::from_str(body)
        .map_err(|error| format!("The host's reply did not parse ({error}): {body}"))
}

fn raw(body: &str, out: &mut dyn Write) -> Result<(), String> {
    writeln!(out, "{}", body.trim_end()).map_err(write_error)
}

fn write_error(error: std::io::Error) -> String {
    format!("Failed to write output: {error}")
}

/// The one destructive command gets a prompt: a restart drops every connected
/// player back to the main menu.
fn confirm_restart(base_url: &str) -> Result<bool, String> {
    eprint!(
        "Restarting the TES3MP server at {base_url} kicks everyone connected. Continue? [y/N] "
    );
    std::io::stderr().flush().map_err(write_error)?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("Failed to read the answer: {error}"))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}
