//! Shared tail of every sign-in: find or create the account, register the
//! device, then mint its session. Runs inside the caller's transaction so a
//! refused invite leaves nothing behind.

use sqlx::PgConnection;

use crate::{
    auth::{
        devices::{self, DeviceIdentity},
        invites, sessions,
        sessions::SessionResponse,
        users,
    },
    error::ApiError,
    AppState,
};

pub async fn sign_in(
    state: &AppState,
    conn: &mut PgConnection,
    email: &str,
    invite_code: Option<&str>,
    device: &DeviceIdentity,
    now: u64,
) -> Result<SessionResponse, ApiError> {
    let keys = &state.keys;
    let user = match users::find_by_email(conn, keys, email).await? {
        Some(user) => user,
        None => {
            let invite =
                invites::authorize_signup(conn, &state.redeem_limiter, invite_code, now).await?;
            let user = users::insert(conn, keys, email, now).await?;
            if let Some(code) = invite {
                invites::mark_used(conn, &code, &user.id, now).await?;
            }
            user
        }
    };
    devices::upsert(conn, keys, &user.id, device, now).await?;
    sessions::create(conn, &user.id, user.email.clone(), &device.id, now).await
}
