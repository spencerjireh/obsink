//! ObSink server: the self-hosted backend for the ObSink clients.
//!
//! Metadata lives in Postgres, encrypted blobs on a filesystem volume, both
//! wrapped with a server-side envelope key. The HTTP contract is spec §4.

pub mod auth;
pub mod blobs;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod retention;
pub mod routes;

use std::sync::Arc;

use sqlx::PgPool;

use crate::{
    auth::{apple::AppleVerifier, email::Mailer, invites::RedeemLimiter},
    blobs::BlobStore,
    config::Config,
    crypto::ServerKeys,
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub keys: Arc<ServerKeys>,
    pub blobs: Arc<BlobStore>,
    pub mailer: Arc<dyn Mailer>,
    pub apple: Arc<AppleVerifier>,
    pub redeem_limiter: Arc<RedeemLimiter>,
}

impl AppState {
    pub fn new(
        config: Config,
        pool: PgPool,
        master_key: [u8; 32],
        mailer: Arc<dyn Mailer>,
    ) -> Self {
        let blobs = Arc::new(BlobStore::new(&config.data_dir));
        let apple = Arc::new(AppleVerifier::new(
            config.apple_jwks_url.clone(),
            config.apple_client_ids.clone(),
        ));
        Self {
            config: Arc::new(config),
            pool,
            keys: Arc::new(ServerKeys::from_master(&master_key)),
            blobs,
            mailer,
            apple,
            redeem_limiter: Arc::new(RedeemLimiter::default()),
        }
    }
}

pub use routes::router;
