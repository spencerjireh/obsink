//! Router assembly plus the two unauthenticated utility routes.

pub mod batch;
pub mod files;
pub mod history;
pub mod me;
pub mod vaults;

use axum::{
    extract::{DefaultBodyLimit, State},
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Serialize;
use tower_http::trace::TraceLayer;

use crate::{auth, error::ApiError, AppState};

#[derive(Serialize)]
pub struct Capabilities {
    pub service: &'static str,
    /// The wire format this server speaks; a client built for another one
    /// shows `Update ObSink` and stops.
    pub protocol: u32,
    pub auth: AuthMethods,
    /// True once the server has any account: new sign-ups then need an invite.
    pub invite_required: bool,
}

#[derive(Serialize)]
pub struct AuthMethods {
    pub email: bool,
    pub apple: bool,
}

async fn capabilities(State(state): State<AppState>) -> Result<Json<Capabilities>, ApiError> {
    let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(Capabilities {
        service: "obsink",
        protocol: obsink_core::PROTOCOL_VERSION,
        auth: AuthMethods {
            email: state.config.email_enabled(),
            apple: state.config.apple_enabled(),
        },
        invite_required: users > 0,
    }))
}

#[derive(Serialize)]
struct Health {
    ok: bool,
}

async fn healthz(State(state): State<AppState>) -> Result<Json<Health>, ApiError> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    Ok(Json(Health { ok: true }))
}

async fn not_found() -> ApiError {
    ApiError::not_found("not_found")
}

pub fn router(state: AppState) -> Router {
    let max_batch = usize::try_from(state.config.max_batch_bytes).unwrap_or(usize::MAX);
    let file_routes = Router::new()
        .route(
            "/vaults/{vault_id}/files/{*path}",
            get(files::get_file)
                .put(files::put_file)
                .delete(files::delete_file),
        )
        .layer(DefaultBodyLimit::disable());
    let batch_routes = Router::new()
        .route("/vaults/{vault_id}/batch", post(batch::batch))
        .layer(DefaultBodyLimit::max(max_batch));
    let history_routes = Router::new()
        .route(
            "/vaults/{vault_id}/history/{*path}",
            get(history::list_versions),
        )
        .route(
            "/vaults/{vault_id}/versions/{name}/{*path}",
            get(history::get_version),
        )
        .route("/vaults/{vault_id}/trash", get(history::list_trash))
        .route("/vaults/{vault_id}/trash/{*path}", get(history::get_trash));

    Router::new()
        .route("/", get(capabilities))
        .route("/healthz", get(healthz))
        .route("/auth/email/start", post(auth::email::start))
        .route("/auth/email/verify", post(auth::email::verify))
        .route("/auth/apple", post(auth::apple::sign_in))
        .route("/auth/me", get(me::me))
        .route("/auth/keys", get(auth::keys::get).put(auth::keys::set))
        .route("/auth/keys/rewrap", put(auth::keys::rewrap))
        .route("/auth/session", delete(me::sign_out))
        .route("/auth/sessions/{session_id}", delete(me::revoke_session))
        .route(
            "/auth/devices/{device_id}",
            patch(me::rename_device).delete(me::revoke_device),
        )
        .route("/auth/account", delete(me::delete_account))
        .route(
            "/auth/invites",
            post(me::create_invite).get(me::list_invites),
        )
        .route("/vaults", get(vaults::list).post(vaults::create))
        .route(
            "/vaults/{vault_id}",
            patch(vaults::rename).delete(vaults::delete_vault),
        )
        .route(
            "/vaults/{vault_id}/devices/self",
            put(vaults::attach_self).delete(vaults::detach_self),
        )
        .route("/vaults/{vault_id}/manifest", get(files::get_manifest))
        .merge(file_routes)
        .merge(batch_routes)
        .merge(history_routes)
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
