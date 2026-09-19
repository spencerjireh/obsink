//! Shared tail of every sign-in: find or create the account, then mint a
//! session. Runs inside the caller's transaction so a refused invite leaves
//! nothing behind.

use sqlx::PgConnection;

use crate::{
    auth::{invites, sessions, sessions::SessionResponse, users},
    error::ApiError,
    AppState,
};

pub enum Identity {
    Email(String),
    Apple { sub: String, email: Option<String> },
}

pub async fn sign_in(
    state: &AppState,
    conn: &mut PgConnection,
    identity: Identity,
    invite_code: Option<&str>,
    device_name: &str,
    now: u64,
) -> Result<SessionResponse, ApiError> {
    let keys = &state.keys;
    let (existing, email, apple_sub) = match &identity {
        Identity::Email(email) => (
            users::find_by_email(conn, keys, email).await?,
            Some(email.clone()),
            None,
        ),
        Identity::Apple { sub, email } => {
            let by_sub = users::find_by_apple_sub(conn, keys, sub).await?;
            let found = match (by_sub, email) {
                (Some(user), _) => Some(user),
                (None, Some(email)) => match users::find_by_email(conn, keys, email).await? {
                    // Same person, new sign-in method: link rather than fork.
                    Some(user) => {
                        users::link_apple(conn, keys, &user.id, sub).await?;
                        Some(user)
                    }
                    None => None,
                },
                (None, None) => None,
            };
            (found, email.clone(), Some(sub.clone()))
        }
    };

    let user = match existing {
        Some(user) => user,
        None => {
            let invite =
                invites::authorize_signup(conn, &state.redeem_limiter, invite_code, now).await?;
            let user =
                users::insert(conn, keys, email.as_deref(), apple_sub.as_deref(), now).await?;
            if let Some(code) = invite {
                invites::mark_used(conn, &code, &user.id, now).await?;
            }
            user
        }
    };
    sessions::create(conn, keys, &user.id, user.email.clone(), device_name, now).await
}
