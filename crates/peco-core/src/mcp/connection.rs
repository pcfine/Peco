//! MCP transport creation helpers.
//!
//! This module provides functions to create [`rmcp`] transports from
//! [`McpServerConfig`].  The transport is then passed to
//! [`McpClientHandler::connect`](super::McpClientHandler::connect), which
//! handles the actual connection lifecycle and tool synchronization.
//!
//! # Transport types
//!
//! | Transport | Function | Returns |
//! |-----------|----------|---------|
//! | stdio (child process) | [`make_stdio_transport`] | [`TokioChildProcess`] |
//! | HTTP (SSE / Streamable) | [`make_http_transport`] | [`StreamableHttpClientTransport`] |

use std::collections::HashMap;

use anyhow::Context;
use http::{HeaderName, HeaderValue};
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use tokio::process::Command;

use crate::config::resolve_env_vars;

// ── Transport factories ────────────────────────────────────────────────────────

/// Build a [`TokioChildProcess`] transport for a stdio MCP server.
///
/// The transport spawns the configured command as a child process and
/// communicates via stdin/stdout.  Environment variables in the command,
/// args, and env map are resolved with [`resolve_env_vars`].
///
/// # Errors
///
/// Returns an error if `command` is missing from the config, or if the
/// child process cannot be created.
pub(crate) fn make_stdio_transport(
    name: &str,
    config: &crate::config::McpServerConfig,
) -> Result<TokioChildProcess, anyhow::Error> {
    let command = config
        .command
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("stdio transport requires 'command' field"))?;

    let resolved_cmd = resolve_env_vars(command);

    let mut cmd = Command::new(&resolved_cmd);
    for arg in &config.args {
        cmd.arg(resolve_env_vars(arg));
    }
    for (key, value) in &config.env {
        let resolved = resolve_env_vars(value);
        cmd.env(key, &resolved);
    }
    // Inherit stderr so the child's error output is visible
    cmd.stderr(std::process::Stdio::inherit());

    TokioChildProcess::new(cmd)
        .with_context(|| format!("Failed to create stdio transport for MCP server '{name}'"))
}

/// Build a [`StreamableHttpClientTransport`] for an HTTP/SSE MCP server.
///
/// Both SSE and Streamable HTTP use the same rmcp transport — the protocol
/// variant is negotiated during the initialize handshake.  Custom headers
/// from the config are resolved and attached.
///
/// # Errors
///
/// Returns an error if `url` is missing from the config, or if a header
/// name or value is invalid.
pub(crate) fn make_http_transport(
    name: &str,
    config: &crate::config::McpServerConfig,
) -> Result<rmcp::transport::StreamableHttpClientTransport<reqwest::Client>, anyhow::Error> {
    let url = config
        .url
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("HTTP transport requires 'url' field"))?;

    let resolved_url = resolve_env_vars(url);

    // Build custom headers from config
    let mut custom_headers = HashMap::new();
    for (key, value) in &config.headers {
        let resolved_value = resolve_env_vars(value);
        let header_name = HeaderName::from_bytes(key.as_bytes())
            .with_context(|| format!("Invalid header name '{key}' in MCP server '{name}'"))?;
        let header_value = HeaderValue::from_str(&resolved_value)
            .with_context(|| format!("Invalid header value for '{key}' in MCP server '{name}'"))?;
        custom_headers.insert(header_name, header_value);
    }

    if !custom_headers.is_empty() {
        tracing::info!(
            server = name,
            header_count = custom_headers.len(),
            "Configuring custom headers for MCP HTTP transport"
        );
    }

    let transport_config = StreamableHttpClientTransportConfig::with_uri(resolved_url.clone())
        .custom_headers(custom_headers);

    Ok(rmcp::transport::StreamableHttpClientTransport::from_config(
        transport_config,
    ))
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_env_vars_in_command() {
        unsafe { std::env::set_var("MCP_TEST_CMD", "/usr/local/bin/node") };
        let resolved = resolve_env_vars("${MCP_TEST_CMD}");
        assert_eq!(resolved, "/usr/local/bin/node");
    }
}
