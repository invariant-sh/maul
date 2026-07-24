//! Pass-through reverse proxy: stream both directions, filter hop-by-hop headers.

mod headers;
mod upstream;

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use reqwest::Client;

use self::headers::{hop_by_hop_filter, strip_content_length};
use self::upstream::build_upstream_url;

/// Forward an inbound request to the configured OpenAI-compatible upstream.
pub async fn reverse_proxy(client: &Client, upstream_base_url: &str, req: Request) -> Response {
    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned());
    let url = build_upstream_url(upstream_base_url, &path_and_query);

    let headers = hop_by_hop_filter(req.headers().clone());
    let body = req.into_body();

    tracing::debug!(%method, %url, "proxying request");

    let upstream_body = reqwest::Body::wrap_stream(body.into_data_stream());

    let upstream_response = match client
        .request(method, &url)
        .headers(headers)
        .body(upstream_body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, %url, "upstream request failed");
            return (
                StatusCode::BAD_GATEWAY,
                format!("maul: upstream error: {error}"),
            )
                .into_response();
        }
    };

    let status = StatusCode::from_u16(upstream_response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let response_headers =
        strip_content_length(hop_by_hop_filter(upstream_response.headers().clone()));
    let response_body = Body::from_stream(upstream_response.bytes_stream());

    let mut response = Response::new(response_body);
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    response
}
