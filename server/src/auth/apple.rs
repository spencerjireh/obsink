//! Sign in with Apple: verify the native identity token (an RS256 JWT) against
//! Apple's JWKS, then find/link/create the account. Works on any self-hosted
//! server because the audience is the ObSink app's bundle id, not a
//! per-server Services ID.

use std::{
    collections::HashSet,
    sync::RwLock,
    time::{Duration, Instant},
};

use axum::{extract::State, http::StatusCode, Json};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

use crate::{
    auth::{
        account, account::Identity, devices, devices::DeviceBody, email, email::normalize_email,
        sessions::SessionResponse,
    },
    db,
    error::{ApiError, AppJson},
    AppState,
};

pub const APPLE_ISSUER: &str = "https://appleid.apple.com";
const JWKS_CACHE_SECS: u64 = 3600;
/// Minimum gap between forced JWKS refreshes for an unknown `kid`. Apple
/// rotates keys rarely; without this, anyone could make every unauthenticated
/// request cost the server an outbound fetch.
const JWKS_FORCED_REFRESH_MIN_SECS: u64 = 60;

#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kid: String,
    pub n: String,
    pub e: String,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<u64>,
    sub: Option<String>,
    email: Option<String>,
}

pub struct AppleVerifier {
    jwks_url: String,
    audiences: Vec<String>,
    http: reqwest::Client,
    cache: RwLock<Option<(Vec<Jwk>, Instant)>>,
    last_forced_refresh: RwLock<Option<Instant>>,
}

pub struct VerifiedIdentity {
    pub sub: String,
    pub email: Option<String>,
}

impl AppleVerifier {
    pub fn new(jwks_url: String, audiences: Vec<String>) -> Self {
        Self {
            jwks_url,
            audiences,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            cache: RwLock::new(None),
            last_forced_refresh: RwLock::new(None),
        }
    }

    pub fn enabled(&self) -> bool {
        !self.audiences.is_empty()
    }

    async fn fetch_jwks(&self) -> Result<Vec<Jwk>, ApiError> {
        let jwks: Jwks = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .and_then(|response| response.error_for_status())
            .map_err(|_| {
                ApiError::status(
                    StatusCode::BAD_GATEWAY,
                    "could not fetch Apple signing keys",
                )
            })?
            .json()
            .await
            .map_err(|_| {
                ApiError::status(
                    StatusCode::BAD_GATEWAY,
                    "could not fetch Apple signing keys",
                )
            })?;
        *self.cache.write().unwrap_or_else(|e| e.into_inner()) =
            Some((jwks.keys.clone(), Instant::now()));
        Ok(jwks.keys)
    }

    async fn key_for(&self, kid: &str) -> Result<Jwk, ApiError> {
        let cached = {
            let guard = self.cache.read().unwrap_or_else(|e| e.into_inner());
            guard
                .as_ref()
                .filter(|(_, fetched)| fetched.elapsed() < Duration::from_secs(JWKS_CACHE_SECS))
                .map(|(keys, _)| keys.clone())
        };
        let keys = match cached {
            Some(keys) => keys,
            None => self.fetch_jwks().await?,
        };
        if let Some(key) = keys.iter().find(|key| key.kid == kid) {
            return Ok(key.clone());
        }
        // Apple rotates keys; one forced refresh before giving up, at most
        // once a minute across all requests.
        let allowed = {
            let mut last = self
                .last_forced_refresh
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let due = last
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(JWKS_FORCED_REFRESH_MIN_SECS));
            if due {
                *last = Some(Instant::now());
            }
            due
        };
        if !allowed {
            return Err(ApiError::unauthorized("unknown Apple signing key"));
        }
        self.fetch_jwks()
            .await?
            .into_iter()
            .find(|key| key.kid == kid)
            .ok_or_else(|| ApiError::unauthorized("unknown Apple signing key"))
    }

    pub async fn verify(&self, token: &str, now: u64) -> Result<VerifiedIdentity, ApiError> {
        if token.split('.').count() != 3 {
            return Err(ApiError::unauthorized("malformed identity token"));
        }
        let header =
            decode_header(token).map_err(|_| ApiError::unauthorized("malformed identity token"))?;
        if header.alg != Algorithm::RS256 {
            return Err(ApiError::unauthorized("unsupported identity token"));
        }
        let kid = header
            .kid
            .ok_or_else(|| ApiError::unauthorized("unsupported identity token"))?;
        let jwk = self.key_for(&kid).await?;
        let key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
            .map_err(|_| ApiError::unauthorized("unknown Apple signing key"))?;

        // Claims are checked by hand below so each failure keeps its own message.
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        validation.required_spec_claims = HashSet::new();
        let data = decode::<Claims>(token, &key, &validation).map_err(|error| {
            use jsonwebtoken::errors::ErrorKind;
            match error.kind() {
                ErrorKind::InvalidSignature => {
                    ApiError::unauthorized("identity token signature is invalid")
                }
                _ => ApiError::unauthorized("malformed identity token"),
            }
        })?;
        let claims = data.claims;
        if claims.iss.as_deref() != Some(APPLE_ISSUER) {
            return Err(ApiError::unauthorized("identity token issuer mismatch"));
        }
        let audience_ok = match &claims.aud {
            Some(Audience::One(aud)) => self.audiences.iter().any(|a| a == aud),
            Some(Audience::Many(auds)) => auds.iter().any(|aud| self.audiences.contains(aud)),
            None => false,
        };
        if !audience_ok {
            return Err(ApiError::unauthorized("identity token audience mismatch"));
        }
        if !claims.exp.is_some_and(|exp| exp > now) {
            return Err(ApiError::unauthorized("identity token expired"));
        }
        let sub = claims
            .sub
            .filter(|sub| !sub.is_empty())
            .ok_or_else(|| ApiError::unauthorized("identity token has no subject"))?;
        Ok(VerifiedIdentity {
            sub,
            email: claims
                .email
                .and_then(|email| normalize_email(Some(&email)).ok()),
        })
    }
}

/// Sent when the identity token carries no email claim and the body offers
/// one without a code. Clients branch on this text to prompt for a code.
pub const EMAIL_VERIFICATION_REQUIRED: &str =
    "email verification required: request a code for this address and retry with `code`";

#[derive(Deserialize)]
pub struct AppleBody {
    pub identity_token: Option<String>,
    /// The signing-in device (spec §4.1); required, so a client from before
    /// wire format v3 is told to update rather than signed in.
    pub device: Option<DeviceBody>,
    /// Apple only includes the email in the first-ever token for an app; the
    /// client can forward the credential's email so linking still works. It
    /// is unverified, so it is only honoured together with a one-time `code`
    /// from `/auth/email/start` for that address.
    pub email: Option<String>,
    /// The one-time code that proves ownership of `email`.
    pub code: Option<String>,
    pub invite_code: Option<String>,
}

pub async fn sign_in(
    State(state): State<AppState>,
    AppJson(body): AppJson<AppleBody>,
) -> Result<Json<SessionResponse>, ApiError> {
    let token = body
        .identity_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::bad_request("identity_token is required"))?;
    if !state.apple.enabled() {
        return Err(ApiError::status(
            StatusCode::SERVICE_UNAVAILABLE,
            "Sign in with Apple is not configured on this server",
        ));
    }
    let now = db::now();
    let identity = state.apple.verify(token, now).await?;
    let device = devices::required(body.device)?;
    let hint = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|hint| !hint.is_empty());

    let mut tx = state.pool.begin().await?;
    // The token's email claim is Apple-verified. A body hint is not: anyone
    // with an Apple ID could name a victim's address and get linked to their
    // account, so the hint counts only once a one-time code proves it.
    let (email, verified_hint) = match (identity.email, hint) {
        (Some(email), _) => (Some(email), None),
        (None, Some(hint)) => {
            let email = normalize_email(Some(hint))?;
            let Some(code) = body.code.as_deref() else {
                return Err(ApiError::forbidden(EMAIL_VERIFICATION_REQUIRED));
            };
            let code = email::parse_code(Some(code))?;
            if let email::CodeCheck::Rejected(error) =
                email::check_code(&state, &mut tx, &email, &code, now).await?
            {
                tx.commit().await?;
                return Err(error);
            }
            (Some(email.clone()), Some(email))
        }
        (None, None) => (None, None),
    };

    let session = account::sign_in(
        &state,
        &mut tx,
        Identity::Apple {
            sub: identity.sub,
            email,
        },
        body.invite_code.as_deref(),
        &device,
        now,
    )
    .await?;
    if let Some(email) = verified_hint {
        email::consume_code(&state, &mut tx, &email).await?;
    }
    tx.commit().await?;
    Ok(Json(session))
}
