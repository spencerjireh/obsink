//! Integration-test harness: a throwaway database per test, a temp blob dir,
//! and the real router on an ephemeral port.
//!
//! Tests are gated on `DATABASE_URL` (an admin URL with CREATE DATABASE
//! rights). Without it they print a skip and pass, so `cargo test --workspace`
//! stays green on machines without Postgres; CI sets
//! `OBSINK_TEST_REQUIRE_DB=1` so a missing database fails loudly.
#![allow(dead_code)]

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use obsink_server::{
    auth::email::{MailFuture, Mailer},
    config::{
        Config, DEFAULT_MAX_BATCH_BYTES, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_VAULTS_PER_USER,
        DEFAULT_MAX_VAULT_BYTES,
    },
    db, router, AppState,
};
use sqlx::{postgres::PgPoolOptions, PgPool};

pub const API_KEY: &str = "secret";
pub const APPLE_AUDIENCE: &str = "com.obsink.ios";

#[derive(Default)]
pub struct RecordingMailer {
    pub sent: Mutex<Vec<(String, String, String)>>,
    pub fail: AtomicBool,
}

impl Mailer for RecordingMailer {
    fn send(&self, to: &str, subject: &str, text: &str) -> MailFuture<'_> {
        let record = (to.to_string(), subject.to_string(), text.to_string());
        Box::pin(async move {
            if self.fail.load(Ordering::SeqCst) {
                return Err("simulated smtp failure".to_string());
            }
            self.sent.lock().unwrap().push(record);
            Ok(())
        })
    }
}

pub struct TestEnv {
    pub state: AppState,
    pub base_url: String,
    pub http: reqwest::Client,
    pub mailer: Arc<RecordingMailer>,
    pub data_dir: tempfile::TempDir,
    admin_url: String,
    db_name: String,
    server: tokio::task::JoinHandle<()>,
}

pub fn test_config(data_dir: &std::path::Path, database_url: String) -> Config {
    Config {
        listen: String::new(),
        database_url,
        data_dir: data_dir.to_path_buf(),
        server_key: None,
        api_key: Some(API_KEY.to_string()),
        apple_client_ids: vec![APPLE_AUDIENCE.to_string()],
        apple_jwks_url: "http://127.0.0.1:9/keys".to_string(),
        smtp: None,
        dev_return_code: true,
        max_vaults_per_user: DEFAULT_MAX_VAULTS_PER_USER,
        max_vault_bytes: DEFAULT_MAX_VAULT_BYTES,
        max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        max_batch_bytes: DEFAULT_MAX_BATCH_BYTES,
        retention_interval_secs: 86_400,
        migrate_on_start: false,
    }
}

/// True when integration tests will run (used to skip expensive fixtures).
pub fn db_available() -> bool {
    std::env::var("DATABASE_URL")
        .map(|url| !url.is_empty())
        .unwrap_or(false)
}

impl TestEnv {
    pub async fn try_new() -> Option<TestEnv> {
        Self::try_with(|_| {}).await
    }

    pub async fn try_with(customize: impl FnOnce(&mut Config)) -> Option<TestEnv> {
        let Some(admin_url) = std::env::var("DATABASE_URL")
            .ok()
            .filter(|url| !url.is_empty())
        else {
            if std::env::var("OBSINK_TEST_REQUIRE_DB").as_deref() == Ok("1") {
                panic!("DATABASE_URL is unset but OBSINK_TEST_REQUIRE_DB=1");
            }
            eprintln!("skipping: DATABASE_URL unset");
            return None;
        };
        let db_name = format!("obsink_test_{}", uuid_suffix());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .expect("connect to DATABASE_URL");
        sqlx::query(&format!("CREATE DATABASE {db_name}"))
            .execute(&admin)
            .await
            .expect("create test database");
        admin.close().await;

        let test_url = replace_db_name(&admin_url, &db_name);
        let pool: PgPool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&test_url)
            .await
            .expect("connect to test database");
        db::migrate(&pool).await.expect("migrate");

        let data_dir = tempfile::tempdir().expect("tempdir");
        let mut config = test_config(data_dir.path(), test_url);
        customize(&mut config);
        let mailer = Arc::new(RecordingMailer::default());
        let state = AppState::new(config, pool, [42u8; 32], mailer.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr: SocketAddr = listener.local_addr().expect("addr");
        let app = router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        Some(TestEnv {
            state,
            base_url: format!("http://{addr}"),
            http: reqwest::Client::new(),
            mailer,
            data_dir,
            admin_url,
            db_name,
            server,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(method, self.url(path))
    }

    pub fn operator(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.req(method, path).bearer_auth(API_KEY)
    }

    pub fn with_token(
        &self,
        token: &str,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        self.req(method, path).bearer_auth(token)
    }

    pub fn api_client(&self, bearer: &str, vault_id: &str) -> obsink_core::ApiClient {
        obsink_core::ApiClient::new(obsink_core::VaultConfig {
            server_url: self.base_url.clone(),
            api_key: bearer.to_string(),
            vault_id: vault_id.to_string(),
            local_path: String::new(),
        })
    }

    pub fn auth_client(&self) -> obsink_core::AuthClient {
        obsink_core::AuthClient::new(&self.base_url)
    }

    /// Create a vault as `bearer`; returns its id.
    pub async fn create_vault(&self, bearer: &str, name: &str) -> String {
        let response = self
            .with_token(bearer, reqwest::Method::POST, "/vaults")
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
        response.json::<serde_json::Value>().await.unwrap()["vault"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Sign in with the email code flow (dev server returns the code inline).
    pub async fn email_sign_in(
        &self,
        email: &str,
        device: &str,
        invite_code: Option<&str>,
    ) -> reqwest::Response {
        let start = self
            .req(reqwest::Method::POST, "/auth/email/start")
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .unwrap();
        assert_eq!(start.status(), 200, "{}", start.text().await.unwrap());
        let code = start.json::<serde_json::Value>().await.unwrap()["code"]
            .as_str()
            .unwrap()
            .to_string();
        self.req(reqwest::Method::POST, "/auth/email/verify")
            .json(&serde_json::json!({
                "email": email, "code": code, "device_name": device, "invite_code": invite_code
            }))
            .send()
            .await
            .unwrap()
    }

    pub async fn email_token(
        &self,
        email: &str,
        device: &str,
        invite_code: Option<&str>,
    ) -> String {
        let response = self.email_sign_in(email, device, invite_code).await;
        assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
        response.json::<serde_json::Value>().await.unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Clear the 60 s resend cooldown so a test can sign the same email in again.
    pub async fn clear_email_cooldown(&self, email: &str) {
        sqlx::query("DELETE FROM email_codes WHERE email_hmac = $1")
            .bind(
                self.state
                    .keys
                    .index("email", &email.trim().to_ascii_lowercase()),
            )
            .execute(&self.state.pool)
            .await
            .unwrap();
    }

    pub async fn table_count(&self, table: &str) -> i64 {
        let (count,): (i64,) = sqlx::query_as(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&self.state.pool)
            .await
            .unwrap();
        count
    }

    /// Drop the database. Call at the end of a passing test.
    pub async fn finish(self) {
        self.server.abort();
        self.state.pool.close().await;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .expect("connect to DATABASE_URL");
        let _ = sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.db_name
        ))
        .execute(&admin)
        .await;
        admin.close().await;
    }
}

fn uuid_suffix() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 6];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Swap the database name in a Postgres URL, keeping user/host/query.
fn replace_db_name(url: &str, db_name: &str) -> String {
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    // postgres://user:pass@host:port/db
    let scheme_end = head.find("://").map(|i| i + 3).unwrap_or(0);
    let path_start = head[scheme_end..].find('/').map(|i| scheme_end + i);
    let base = match path_start {
        Some(i) => &head[..i],
        None => head,
    };
    match query {
        Some(query) => format!("{base}/{db_name}?{query}"),
        None => format!("{base}/{db_name}"),
    }
}
