//! Email one-time-code sign-in over SMTP.
//!
//! `POST /auth/email/start` sends a six-digit code (60 s resend cooldown,
//! 10 min validity, 5 attempts); `POST /auth/email/verify` trades it for a
//! session. The mail goes out before the code is recorded, so a failed send
//! does not consume the cooldown.

use std::{future::Future, pin::Pin};

use axum::{extract::State, http::StatusCode, Json};
use lettre::{
    message::header::ContentType, transport::smtp::authentication::Credentials, AsyncSmtpTransport,
    AsyncTransport, Message, Tokio1Executor,
};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};

use crate::{
    auth::{account, account::Identity, devices, devices::DeviceBody, sessions::SessionResponse},
    config::{SmtpConfig, SmtpTls},
    db,
    error::{ApiError, AppJson},
    AppState,
};

pub const OTP_TTL_SECS: u64 = 600;
pub const OTP_RESEND_COOLDOWN_SECS: u64 = 60;
pub const OTP_MAX_ATTEMPTS: i32 = 5;
const MAX_EMAIL_LEN: usize = 254;

pub type MailFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

pub trait Mailer: Send + Sync {
    fn send(&self, to: &str, subject: &str, text: &str) -> MailFuture<'_>;
}

/// No SMTP configured: `/auth/email/start` returns 503 unless the dev flag
/// hands the code back inline.
pub struct NoMailer;

impl Mailer for NoMailer {
    fn send(&self, _to: &str, _subject: &str, _text: &str) -> MailFuture<'_> {
        Box::pin(async { Err("email sending is not configured".to_string()) })
    }
}

pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: String,
}

impl SmtpMailer {
    pub fn new(config: &SmtpConfig) -> Result<Self, String> {
        let mut builder = match config.tls {
            SmtpTls::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host),
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host),
            SmtpTls::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &config.host,
            )),
        }
        .map_err(|error| format!("SMTP_HOST: {error}"))?
        .port(config.port);
        if let (Some(username), Some(password)) = (&config.username, &config.password) {
            builder = builder.credentials(Credentials::new(username.clone(), password.clone()));
        }
        Ok(Self {
            transport: builder.build(),
            from: config.from.clone(),
        })
    }
}

impl Mailer for SmtpMailer {
    fn send(&self, to: &str, subject: &str, text: &str) -> MailFuture<'_> {
        let to = to.to_string();
        let subject = subject.to_string();
        let text = text.to_string();
        Box::pin(async move {
            let message = Message::builder()
                .from(self.from.parse().map_err(|e| format!("SMTP_FROM: {e}"))?)
                .to(to.parse().map_err(|e| format!("recipient: {e}"))?)
                .subject(subject)
                .header(ContentType::TEXT_PLAIN)
                .body(text)
                .map_err(|e| format!("build message: {e}"))?;
            self.transport
                .send(message)
                .await
                .map(|_| ())
                .map_err(|e| format!("smtp: {e}"))
        })
    }
}

// --- Handlers ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct StartBody {
    pub email: Option<String>,
}

#[derive(Serialize)]
pub struct StartResponse {
    pub sent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Deserialize)]
pub struct VerifyBody {
    pub email: Option<String>,
    pub code: Option<String>,
    /// The signing-in device (spec §4.1). A v2 client sends `device_name`
    /// instead and gets a synthesized device until OBS-143.
    pub device: Option<DeviceBody>,
    pub device_name: Option<String>,
    pub invite_code: Option<String>,
}

pub fn normalize_email(value: Option<&str>) -> Result<String, ApiError> {
    let email = value
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let valid = email.len() <= MAX_EMAIL_LEN && {
        let mut parts = email.split('@');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(local), Some(domain), None) => {
                !local.is_empty()
                    && !local.chars().any(char::is_whitespace)
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
                    && !domain.chars().any(char::is_whitespace)
            }
            _ => false,
        }
    };
    if !valid {
        return Err(ApiError::bad_request("a valid email address is required"));
    }
    Ok(email)
}

pub async fn start(
    State(state): State<AppState>,
    AppJson(body): AppJson<StartBody>,
) -> Result<Json<StartResponse>, ApiError> {
    let email = normalize_email(body.email.as_deref())?;
    let email_hmac = state.keys.index("email", &email);
    let now = db::now();

    let last_sent: Option<(i64,)> =
        sqlx::query_as("SELECT last_sent FROM email_codes WHERE email_hmac = $1")
            .bind(&email_hmac)
            .fetch_optional(&state.pool)
            .await?;
    if let Some((last_sent,)) = last_sent {
        if now.saturating_sub(db::to_u64(last_sent)) < OTP_RESEND_COOLDOWN_SECS {
            return Err(ApiError::status(
                StatusCode::TOO_MANY_REQUESTS,
                "a code was sent recently; wait a minute and try again",
            ));
        }
    }
    let smtp_configured = state.config.smtp.is_some();
    if !smtp_configured && !state.config.dev_return_code {
        return Err(ApiError::status(
            StatusCode::SERVICE_UNAVAILABLE,
            "email sign-in is not configured on this server",
        ));
    }

    let code = crate::crypto::random_digits(6);
    if smtp_configured {
        state
            .mailer
            .send(
                &email,
                &format!("{code} is your ObSink sign-in code"),
                &format!(
                    "Your ObSink sign-in code is {code}.\n\nIt expires in 10 minutes. If you did not request it, ignore this email.\n"
                ),
            )
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "sign-in email failed");
                ApiError::status(StatusCode::BAD_GATEWAY, "could not send sign-in email")
            })?;
    }

    sqlx::query(
        "INSERT INTO email_codes (email_hmac, code_hmac, expires, attempts, last_sent)
         VALUES ($1, $2, $3, 0, $4)
         ON CONFLICT (email_hmac) DO UPDATE SET code_hmac = EXCLUDED.code_hmac,
             expires = EXCLUDED.expires, attempts = 0, last_sent = EXCLUDED.last_sent",
    )
    .bind(&email_hmac)
    .bind(state.keys.index("otp", &format!("{email}:{code}")))
    .bind(db::to_i64(now + OTP_TTL_SECS))
    .bind(db::to_i64(now))
    .execute(&state.pool)
    .await?;

    Ok(Json(StartResponse {
        sent: smtp_configured,
        code: state.config.dev_return_code.then_some(code),
    }))
}

/// Outcome of checking a one-time code. A rejection carries the response to
/// send; the attempt counter update it made must still be committed.
pub enum CodeCheck {
    Valid,
    Rejected(ApiError),
}

/// Normalise a submitted code: whitespace stripped, exactly six digits.
pub fn parse_code(raw: Option<&str>) -> Result<String, ApiError> {
    let code: String = raw
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return Err(ApiError::bad_request("code must be 6 digits"));
    }
    Ok(code)
}

/// Compare `code` against the outstanding one for `email`, locking the row.
/// A wrong code increments `attempts` (and the fifth strike voids the code);
/// the caller commits the transaction before returning the rejection so the
/// counter sticks. The code is not consumed here: `consume_code` runs after
/// the sign-in succeeds so a refused invite does not burn it.
pub async fn check_code(
    state: &AppState,
    conn: &mut PgConnection,
    email: &str,
    code: &str,
    now: u64,
) -> Result<CodeCheck, ApiError> {
    let email_hmac = state.keys.index("email", email);
    let row = sqlx::query(
        "SELECT code_hmac, expires, attempts FROM email_codes WHERE email_hmac = $1 FOR UPDATE",
    )
    .bind(&email_hmac)
    .fetch_optional(&mut *conn)
    .await?;
    let (code_hmac, expires, attempts) = match row {
        Some(row) => (
            row.get::<Option<Vec<u8>>, _>("code_hmac"),
            db::to_u64(row.get("expires")),
            row.get::<i32, _>("attempts"),
        ),
        None => {
            return Ok(CodeCheck::Rejected(ApiError::unauthorized(
                "code expired; request a new one",
            )))
        }
    };
    let Some(code_hmac) = code_hmac.filter(|_| expires > now) else {
        return Ok(CodeCheck::Rejected(ApiError::unauthorized(
            "code expired; request a new one",
        )));
    };
    if attempts >= OTP_MAX_ATTEMPTS {
        sqlx::query("UPDATE email_codes SET code_hmac = NULL WHERE email_hmac = $1")
            .bind(&email_hmac)
            .execute(&mut *conn)
            .await?;
        return Ok(CodeCheck::Rejected(ApiError::unauthorized(
            "too many attempts; request a new code",
        )));
    }
    let expected = state.keys.index("otp", &format!("{email}:{code}"));
    if !bool::from(subtle::ConstantTimeEq::ct_eq(
        expected.as_slice(),
        code_hmac.as_slice(),
    )) {
        sqlx::query("UPDATE email_codes SET attempts = attempts + 1 WHERE email_hmac = $1")
            .bind(&email_hmac)
            .execute(&mut *conn)
            .await?;
        return Ok(CodeCheck::Rejected(ApiError::unauthorized(
            "incorrect code",
        )));
    }
    Ok(CodeCheck::Valid)
}

/// Void the outstanding code for `email` (single use), keeping `last_sent`
/// so the resend cooldown survives.
pub async fn consume_code(
    state: &AppState,
    conn: &mut PgConnection,
    email: &str,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE email_codes SET code_hmac = NULL WHERE email_hmac = $1")
        .bind(state.keys.index("email", email))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn verify(
    State(state): State<AppState>,
    AppJson(body): AppJson<VerifyBody>,
) -> Result<Json<SessionResponse>, ApiError> {
    let email = normalize_email(body.email.as_deref())?;
    let code = parse_code(body.code.as_deref())?;
    let now = db::now();

    let mut tx = state.pool.begin().await?;
    if let CodeCheck::Rejected(error) = check_code(&state, &mut tx, &email, &code, now).await? {
        tx.commit().await?;
        return Err(error);
    }

    let device = match body.device {
        Some(device) => device.validate()?,
        None => devices::legacy_device(body.device_name.as_deref()),
    };
    let session = account::sign_in(
        &state,
        &mut tx,
        Identity::Email(email.clone()),
        body.invite_code.as_deref(),
        &device,
        now,
    )
    .await?;
    consume_code(&state, &mut tx, &email).await?;
    tx.commit().await?;
    Ok(Json(session))
}
