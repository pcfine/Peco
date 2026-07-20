use super::ToolError;
use peco_derive::peco_tool;
use tracing::debug;

/// HTTP fetch tool.
///
/// Fetches content from a URL via HTTP. Supports GET, POST, PUT, and DELETE
/// methods. Returns the response body as text.
#[peco_tool(
    name = "fetch",
    description = "Fetch content from a URL via HTTP. Supports GET, POST, PUT, and DELETE methods. Returns the response body as text. Use this to retrieve web pages, call REST APIs, or access any HTTP-accessible resource. The response is returned even on HTTP error status codes (4xx/5xx) so the error can be interpreted.",
    params(
        url = "The URL to fetch. Must include the protocol (http:// or https://).",
        method = "HTTP method: GET, POST, PUT, or DELETE. Defaults to GET if not specified.",
        headers = "Optional JSON object of additional HTTP headers, e.g. '{\"Authorization\": \"Bearer token\", \"Content-Type\": \"application/json\"}'.",
        body = "Optional request body for POST/PUT requests."
    )
)]
pub async fn fetch(
    url: String,
    method: Option<String>,
    headers: Option<String>,
    body: Option<String>,
) -> Result<String, ToolError> {
    let method_str = method.as_deref().unwrap_or("GET").to_uppercase();
    let method = match method_str.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "DELETE" => reqwest::Method::DELETE,
        other => {
            return Err(ToolError::ToolCallError(
                format!("unsupported HTTP method: {other}").into(),
            ));
        }
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("peco/0.1")
        .build()
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

    let mut req = client.request(method.clone(), &url);

    if let Some(ref headers_str) = headers
        && let Ok(header_map) =
            serde_json::from_str::<std::collections::HashMap<String, String>>(headers_str)
    {
        for (key, value) in header_map {
            req = req.header(&key, &value);
        }
    }

    if let Some(ref body_str) = body {
        req = req.body(body_str.clone());
    }

    debug!("Fetching {method} {url}");
    let response = req
        .send()
        .await
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| ToolError::ToolCallError(Box::new(e)))?;

    if status.is_success() {
        Ok(text)
    } else {
        Ok(format!("HTTP {status}\n\n{text}"))
    }
}
