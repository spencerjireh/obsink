mod common;

use std::fs;

use common::TestEnv;
use obsink_server::{
    blobs::Tier,
    retention::{self, ORPHAN_GRACE_SECS, TRASH_RETENTION_SECS, VERSION_RETENTION_SECS},
};

const VAULT: &str = "vault_00000000-0000-4000-8000-000000000000";

fn seed(env: &TestEnv, tier: Tier, vault: &str, path: &str, timestamps: &[u64]) {
    for ts in timestamps {
        env.state.blobs.put_live(vault, path, b"x").unwrap();
        match tier {
            Tier::Versions => env.state.blobs.archive_version(vault, path, *ts).unwrap(),
            Tier::Trash => env.state.blobs.move_to_trash(vault, path, *ts).unwrap(),
            Tier::Live => {}
        }
    }
}

#[tokio::test]
async fn prunes_versions_beyond_the_newest_ten_per_file() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let vault = env.create_vault(env.owner_token(), "v").await;
    let timestamps: Vec<u64> = (1..=12).map(|i| 1_000_000 + i).collect();
    seed(&env, Tier::Versions, &vault, "note", &timestamps);
    let report = retention::run_once(&env.state, 1_000_100).await.unwrap();
    assert_eq!(report.versions_removed, 2);
    let remaining = env
        .state
        .blobs
        .list_history(Tier::Versions, &vault, "note")
        .unwrap();
    assert_eq!(remaining.len(), 10);
    assert!(!remaining.contains(&"1000001".to_string()));
    assert!(!remaining.contains(&"1000002".to_string()));
    assert!(remaining.contains(&"1000012".to_string()));
    env.finish().await;
}

#[tokio::test]
async fn prunes_versions_older_than_the_retention_window() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let vault = env.create_vault(env.owner_token(), "v").await;
    let now = VERSION_RETENTION_SECS + 5;
    seed(
        &env,
        Tier::Versions,
        &vault,
        "note",
        &[1, VERSION_RETENTION_SECS + 1],
    );
    let report = retention::run_once(&env.state, now).await.unwrap();
    assert_eq!(report.versions_removed, 1);
    assert_eq!(
        env.state
            .blobs
            .list_history(Tier::Versions, &vault, "note")
            .unwrap(),
        vec![(VERSION_RETENTION_SECS + 1).to_string()]
    );
    env.finish().await;
}

#[tokio::test]
async fn prunes_trash_entries_older_than_the_retention_window() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let vault = env.create_vault(env.owner_token(), "v").await;
    let now = TRASH_RETENTION_SECS + 5;
    seed(
        &env,
        Tier::Trash,
        &vault,
        "note",
        &[1, TRASH_RETENTION_SECS + 1],
    );
    let report = retention::run_once(&env.state, now).await.unwrap();
    assert_eq!(report.trash_removed, 1);
    assert_eq!(
        env.state
            .blobs
            .list_history(Tier::Trash, &vault, "note")
            .unwrap(),
        vec![(TRASH_RETENTION_SECS + 1).to_string()]
    );
    // Fully pruned history directories disappear.
    seed(&env, Tier::Trash, &vault, "gone", &[1]);
    retention::run_once(&env.state, now).await.unwrap();
    assert!(
        !env.state.blobs.tier_root(Tier::Trash).join(&vault).exists()
            || fs::read_dir(env.state.blobs.tier_root(Tier::Trash).join(&vault))
                .unwrap()
                .count()
                == 1
    );
    env.finish().await;
}

#[tokio::test]
async fn prunes_expired_sessions_and_codes() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let token = env.invited("r@example.com", "d").await.token;
    sqlx::query("UPDATE sessions SET expires = 1")
        .execute(&env.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE email_codes SET last_sent = 1")
        .execute(&env.state.pool)
        .await
        .unwrap();
    let report = retention::run_once(&env.state, obsink_server::db::now())
        .await
        .unwrap();
    assert_eq!(report.sessions_removed, 2);
    assert_eq!(report.codes_removed, 2);
    assert_eq!(env.table_count("sessions").await, 0);
    assert_eq!(env.table_count("email_codes").await, 0);
    // Devices outlive their sessions: the user sees and removes them.
    assert_eq!(env.table_count("devices").await, 2);
    let _ = token;
    env.finish().await;
}

/// Backdate a vault directory past the orphan grace period.
fn age_vault_dir(env: &TestEnv, tier: Tier, vault: &str) {
    let dir = env.state.blobs.tier_root(tier).join(vault);
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(ORPHAN_GRACE_SECS + 60);
    fs::File::open(&dir).unwrap().set_modified(old).unwrap();
}

#[tokio::test]
async fn removes_orphaned_vault_directories() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    let real = env.create_vault(env.owner_token(), "real").await;
    env.state.blobs.put_live(&real, "tok", b"x").unwrap();
    seed(&env, Tier::Trash, VAULT, "tok", &[1]);
    env.state.blobs.put_live(VAULT, "tok", b"x").unwrap();

    // A directory younger than the grace period may belong to a vault whose
    // row is still being written; it survives this pass.
    let report = retention::run_once(&env.state, 10).await.unwrap();
    assert_eq!(report.orphan_dirs_removed, 0);
    assert!(env.state.blobs.live_exists(VAULT, "tok"));

    age_vault_dir(&env, Tier::Live, VAULT);
    age_vault_dir(&env, Tier::Trash, VAULT);
    age_vault_dir(&env, Tier::Live, &real);
    let report = retention::run_once(&env.state, 10).await.unwrap();
    assert_eq!(report.orphan_dirs_removed, 2);
    assert!(env.state.blobs.live_exists(&real, "tok"));
    assert!(!env.state.blobs.live_exists(VAULT, "tok"));
    assert_eq!(
        env.state.blobs.vault_dirs(Tier::Trash).unwrap(),
        Vec::<String>::new()
    );
    env.finish().await;
}

#[tokio::test]
async fn clears_expired_approval_requests_and_keeps_live_ones() {
    let Some(env) = TestEnv::try_with_owner().await else {
        return;
    };
    env.clear_email_cooldown(common::OWNER_EMAIL).await;
    let stale = env.sign_in(common::OWNER_EMAIL, "stale", None).await;
    env.clear_email_cooldown(common::OWNER_EMAIL).await;
    let live = env.sign_in(common::OWNER_EMAIL, "live", None).await;
    for account in [&stale, &live] {
        let response = env
            .with_token(&account.token, reqwest::Method::PUT, "/auth/approval")
            .json(&serde_json::json!({
                "public_key": obsink_core::encode_base64(
                    obsink_core::new_approval_request().public_key()
                )
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201);
    }
    sqlx::query("UPDATE devices SET approval_expires = 1 WHERE id = 'stale'")
        .execute(&env.state.pool)
        .await
        .unwrap();
    let report = retention::run_once(&env.state, obsink_server::db::now())
        .await
        .unwrap();
    assert_eq!(report.approvals_expired, 1);
    let (cleared,): (bool,) = sqlx::query_as(
        "SELECT approval_public_key IS NULL AND approval_requested IS NULL
             AND approval_expires IS NULL AND approval_wrapped IS NULL
             AND approval_approved_by IS NULL AND approval_approved IS NULL
         FROM devices WHERE id = 'stale'",
    )
    .fetch_one(&env.state.pool)
    .await
    .unwrap();
    assert!(cleared);
    let status: serde_json::Value = env
        .with_token(&live.token, reqwest::Method::GET, "/auth/approval")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(status["approval"]["public_key"].is_string());
    assert_eq!(
        retention::run_once(&env.state, obsink_server::db::now())
            .await
            .unwrap()
            .approvals_expired,
        0
    );
    env.finish().await;
}
