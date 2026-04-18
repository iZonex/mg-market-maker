//! Client onboarding API (Epic 1 item 1.7).
//!
//! Admin endpoints for creating, listing, and querying clients.
//! Note: creating a client registers it in DashboardState but
//! does NOT spawn engines — that requires a restart.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mm_common::config::ProductType;
use serde::{Deserialize, Serialize};

use crate::state::DashboardState;

/// Admin client management routes.
pub fn admin_client_routes() -> Router<DashboardState> {
    Router::new()
        .route("/api/admin/clients", post(create_client))
        .route("/api/admin/clients", get(list_clients))
        .route("/api/admin/clients/{id}", get(get_client))
}

#[derive(Debug, Deserialize)]
pub struct CreateClientRequest {
    pub id: String,
    pub name: String,
    pub symbols: Vec<String>,
    #[serde(default)]
    pub webhook_urls: Vec<String>,
    /// Epic 40.10 — ISO 3166-1 alpha-2 country code or `"global"`.
    /// Drives product gating; `"US"` blocks perp products. Default
    /// `"global"` preserves legacy behaviour for existing API
    /// consumers.
    #[serde(default = "default_jurisdiction_ingress")]
    pub jurisdiction: String,
}

fn default_jurisdiction_ingress() -> String {
    "global".to_string()
}

#[derive(Debug, Serialize)]
pub struct ClientResponse {
    pub id: String,
    pub name: Option<String>,
    pub symbols: Vec<String>,
    pub registered: bool,
}

async fn create_client(
    State(state): State<DashboardState>,
    Json(req): Json<CreateClientRequest>,
) -> Response {
    // Epic 40.10 — jurisdiction gate. Fail-closed on US + perp.
    // We only know the engine's product here, so gate against it
    // at registration time: if this engine is running a perp
    // product and the client claims US jurisdiction, the client
    // is refused entirely — no partial registration.
    let j = req.jurisdiction.to_ascii_uppercase();
    let engine_product = state.engine_product();
    if j == "US"
        && matches!(
            engine_product,
            Some(ProductType::LinearPerp) | Some(ProductType::InversePerp)
        )
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "jurisdiction_forbidden",
                "message": "US-jurisdiction clients cannot be registered on a perp engine.\
                            Use a spot engine or set jurisdiction != US.",
                "client_id": req.id,
                "jurisdiction": req.jurisdiction,
                "engine_product": engine_product.map(|p| p.label()),
            })),
        )
            .into_response();
    }

    // Check for duplicate client ID.
    let existing_ids = state.client_ids();
    if existing_ids.contains(&req.id) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "duplicate_client_id",
                "client_id": req.id,
            })),
        )
            .into_response();
    }

    // Register client and its symbols.
    state.register_client(&req.id, &req.symbols);

    // Set up webhook dispatcher if URLs provided.
    if !req.webhook_urls.is_empty() {
        let wh = crate::webhooks::WebhookDispatcher::new();
        for url in &req.webhook_urls {
            wh.add_url(url.clone());
        }
        state.set_client_webhook_dispatcher(&req.id, wh);
    }

    Json(ClientResponse {
        id: req.id,
        name: Some(req.name),
        symbols: req.symbols,
        registered: true,
    })
    .into_response()
}

async fn list_clients(State(state): State<DashboardState>) -> Json<Vec<ClientResponse>> {
    let ids = state.client_ids();
    let clients = ids
        .into_iter()
        .map(|id| {
            let syms = state.get_client_symbols(&id);
            ClientResponse {
                id,
                name: None,
                symbols: syms.into_iter().map(|s| s.symbol).collect(),
                registered: true,
            }
        })
        .collect();
    Json(clients)
}

async fn get_client(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> Json<ClientResponse> {
    let syms = state.get_client_symbols(&id);
    let registered = state.client_ids().contains(&id);
    Json(ClientResponse {
        id,
        name: None,
        symbols: syms.into_iter().map(|s| s.symbol).collect(),
        registered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_client_request_deserializes() {
        let json = r#"{"id":"acme","name":"Acme Corp","symbols":["BTCUSDT","ETHUSDT"]}"#;
        let req: CreateClientRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, "acme");
        assert_eq!(req.symbols.len(), 2);
    }

    #[test]
    fn client_response_serializes() {
        let resp = ClientResponse {
            id: "acme".into(),
            name: Some("Acme".into()),
            symbols: vec!["BTCUSDT".into()],
            registered: true,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("acme"));
        assert!(json.contains("registered"));
    }
}
