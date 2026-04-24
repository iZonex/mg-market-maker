//! SEC-1 regression — every controller route that was
//! previously anonymous now returns 401 without auth, 403 for
//! the wrong role, and passes auth (200 / 415 / 422) for the
//! right role. The 2026-04-21 product-journey smoke caught
//! `/api/v1/fleet`, `/api/v1/vault`, `/api/v1/approvals`,
//! `/api/v1/agents/*` all reachable without a Bearer token.
//!
//! Future refactors that drop the auth layer from any of the
//! three tiers (internal_view / control / admin) fail here
//! before they ship.

use axum::http::StatusCode;
use std::net::SocketAddr;
use std::time::Duration;

use mm_controller::{http_router_full_authed, AgentRegistry, FleetState};
use mm_dashboard::auth::{AuthState, Role};

#[tokio::test]
async fn sec1_controller_routes_require_auth_and_respect_role_tiers() {
    let fleet = FleetState::new();
    let registry = AgentRegistry::new();
    let auth = AuthState::new("0123456789abcdef0123456789abcdef0123456789abcdef");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let app = http_router_full_authed(fleet, registry, None, None, None, auth.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let base = format!("http://{addr}");
    let http = reqwest::Client::builder().build().unwrap();

    // Seed role tokens directly on the AuthState.
    let admin = auth
        .create_password_user("admin", "password12", Role::Admin)
        .expect("admin");
    let op = auth
        .create_password_user("op", "password12", Role::Operator)
        .expect("op");
    let view = auth
        .create_password_user("view", "password12", Role::Viewer)
        .expect("view");
    let tenant = auth
        .create_client_reader("acme-user", "password12", "acme")
        .expect("tenant");

    let admin_tok = auth.generate_token(&admin);
    let op_tok = auth.generate_token(&op);
    let view_tok = auth.generate_token(&view);
    let tenant_tok = auth.generate_token(&tenant);

    // 1) Anonymous attacker — every /api/v1/* route 401.
    for path in [
        "/api/v1/fleet",
        "/api/v1/vault",
        "/api/v1/approvals",
        "/api/v1/tunables",
        "/api/v1/templates",
    ] {
        let r = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "anon GET {path}");
    }
    for path in [
        "/api/v1/vault",
        "/api/v1/agents/x/deployments",
        "/api/v1/approvals/deadbeef/accept",
    ] {
        let r = http
            .post(format!("{base}{path}"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "anon POST {path}");
    }

    // 2) Read tier (internal_view) — admin/op/view=200, CR=403.
    for path in [
        "/api/v1/fleet",
        "/api/v1/vault",
        "/api/v1/approvals",
        "/api/v1/tunables",
        "/api/v1/templates",
    ] {
        for (label, tok, expected) in [
            ("admin", &admin_tok, StatusCode::OK),
            ("op", &op_tok, StatusCode::OK),
            ("view", &view_tok, StatusCode::OK),
            ("cr", &tenant_tok, StatusCode::FORBIDDEN),
        ] {
            let r = http
                .get(format!("{base}{path}"))
                .bearer_auth(tok)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), expected, "{label} GET {path}");
        }
    }

    // 3) Control tier — admin+op pass through; viewer+cr 403.
    // Body-validation status (415/422) after the middleware is
    // fine — we only prove the auth/role gate doesn't block.
    for (label, tok, blocked) in [
        ("admin", &admin_tok, false),
        ("op", &op_tok, false),
        ("view", &view_tok, true),
        ("cr", &tenant_tok, true),
    ] {
        let r = http
            .post(format!("{base}/api/v1/agents/x/deployments"))
            .bearer_auth(tok)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        if blocked {
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "{label} POST deploy");
        } else {
            assert!(
                r.status() != StatusCode::UNAUTHORIZED && r.status() != StatusCode::FORBIDDEN,
                "{label} POST deploy passed auth? got {}",
                r.status()
            );
        }
    }

    // 4) Admin-only tier — admin passes, op joins the blocked
    // side with view+cr.
    for path in [
        "/api/v1/vault",
        "/api/v1/approvals/deadbeef/accept",
        "/api/v1/approvals/pre-approve",
    ] {
        for (label, tok, blocked) in [
            ("admin", &admin_tok, false),
            ("op", &op_tok, true),
            ("view", &view_tok, true),
            ("cr", &tenant_tok, true),
        ] {
            let r = http
                .post(format!("{base}{path}"))
                .bearer_auth(tok)
                .json(&serde_json::json!({}))
                .send()
                .await
                .unwrap();
            if blocked {
                assert_eq!(r.status(), StatusCode::FORBIDDEN, "{label} POST {path}");
            } else {
                assert!(
                    r.status() != StatusCode::UNAUTHORIZED && r.status() != StatusCode::FORBIDDEN,
                    "{label} POST {path} passed auth? got {}",
                    r.status()
                );
            }
        }
    }

    // 5) PUT tunables — admin-only write.
    for (label, tok, blocked) in [
        ("admin", &admin_tok, false),
        ("op", &op_tok, true),
        ("view", &view_tok, true),
        ("cr", &tenant_tok, true),
    ] {
        let r = http
            .put(format!("{base}/api/v1/tunables"))
            .bearer_auth(tok)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        if blocked {
            assert_eq!(r.status(), StatusCode::FORBIDDEN, "{label} PUT tunables");
        } else {
            assert!(
                r.status() != StatusCode::UNAUTHORIZED && r.status() != StatusCode::FORBIDDEN,
                "{label} PUT tunables passed auth? got {}",
                r.status()
            );
        }
    }

    server.abort();
}
