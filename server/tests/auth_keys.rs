mod common;

use common::TestEnv;
use obsink_core::{rewrap_account_key, unlock_account_key, AccountKeyMaterial};
use reqwest::Method;

fn material_json(material: &AccountKeyMaterial) -> serde_json::Value {
    serde_json::json!({
        "wrapped": material.wrapped_b64(),
        "salt": material.salt_b64(),
        "verifier": material.verifier_b64(),
    })
}

#[tokio::test]
async fn the_first_set_wins_and_a_second_device_unlocks_with_it() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let mac = env.sign_in("keys@example.com", "mac", None).await;
    let before: serde_json::Value = env
        .with_token(&mac.token, Method::GET, "/auth/keys")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(before["account_key"].is_null());

    let (key, material) = env.set_passphrase(&mac, "correct horse battery").await;

    // A second device that raced to set its own key gets the winner's blob.
    env.clear_email_cooldown("keys@example.com").await;
    let phone = env.sign_in("keys@example.com", "phone", None).await;
    let (_, losing) = obsink_core::create_account_key("another passphrase", &mac.user_id).unwrap();
    let race = env
        .with_token(&phone.token, Method::PUT, "/auth/keys")
        .json(&material_json(&losing))
        .send()
        .await
        .unwrap();
    assert_eq!(race.status(), 409);
    let race: serde_json::Value = race.json().await.unwrap();
    assert_eq!(race["account_key"]["wrapped"], material.wrapped_b64());
    assert!(race["account_key"]["key_id"]
        .as_str()
        .unwrap()
        .starts_with("key_"));
    assert!(race.get("verifier").is_none() && race["account_key"].get("verifier").is_none());

    // GET returns the same blob; the phone unlocks with the passphrase.
    let got: serde_json::Value = env
        .with_token(&phone.token, Method::GET, "/auth/keys")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let salt = obsink_core::decode_base64(got["account_key"]["salt"].as_str().unwrap()).unwrap();
    let wrapped =
        obsink_core::decode_base64(got["account_key"]["wrapped"].as_str().unwrap()).unwrap();
    assert_eq!(
        unlock_account_key("correct horse battery", &salt, &wrapped, &mac.user_id).unwrap(),
        key
    );
    assert!(unlock_account_key("wrong", &salt, &wrapped, &mac.user_id).is_err());
    env.finish().await;
}

#[tokio::test]
async fn rewrap_needs_the_verifier_and_keeps_the_key() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let mac = env.sign_in("rewrap@example.com", "mac", None).await;

    // No passphrase yet: nothing to rewrap.
    let (_, stray) = obsink_core::create_account_key("x", &mac.user_id).unwrap();
    let early = env
        .with_token(&mac.token, Method::PUT, "/auth/keys/rewrap")
        .json(&material_json(&stray))
        .send()
        .await
        .unwrap();
    assert_eq!(early.status(), 400);

    let (key, material) = env.set_passphrase(&mac, "correct horse battery").await;
    let key_id = env
        .with_token(&mac.token, Method::GET, "/auth/keys")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["account_key"]["key_id"]
        .as_str()
        .unwrap()
        .to_string();

    // A stolen session cannot lock the owner out: a fresh key has another verifier.
    let (_, imposter) = obsink_core::create_account_key("attacker", &mac.user_id).unwrap();
    let refused = env
        .with_token(&mac.token, Method::PUT, "/auth/keys/rewrap")
        .json(&material_json(&imposter))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    assert_eq!(
        refused.json::<serde_json::Value>().await.unwrap()["error"],
        "passphrase does not match this account"
    );

    // The real client rewraps the same key under a new passphrase.
    let rewrapped = rewrap_account_key(&key, "new passphrase here", &mac.user_id).unwrap();
    assert_eq!(rewrapped.verifier, material.verifier);
    let ok = env
        .with_token(&mac.token, Method::PUT, "/auth/keys/rewrap")
        .json(&material_json(&rewrapped))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 204);
    let got: serde_json::Value = env
        .with_token(&mac.token, Method::GET, "/auth/keys")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        got["account_key"]["key_id"], key_id,
        "the key id never changes"
    );
    assert_eq!(got["account_key"]["wrapped"], rewrapped.wrapped_b64());
    let salt = obsink_core::decode_base64(got["account_key"]["salt"].as_str().unwrap()).unwrap();
    let wrapped =
        obsink_core::decode_base64(got["account_key"]["wrapped"].as_str().unwrap()).unwrap();
    assert_eq!(
        unlock_account_key("new passphrase here", &salt, &wrapped, &mac.user_id).unwrap(),
        key
    );
    assert!(unlock_account_key("correct horse battery", &salt, &wrapped, &mac.user_id).is_err());
    env.finish().await;
}

#[tokio::test]
async fn malformed_material_is_refused() {
    let Some(env) = TestEnv::try_new().await else {
        return;
    };
    let mac = env.sign_in("bad@example.com", "mac", None).await;
    for (body, message) in [
        (
            serde_json::json!({ "salt": "AAAAAAAAAAAAAAAAAAAAAA==", "verifier": "" }),
            "wrapped is required",
        ),
        (
            serde_json::json!({ "wrapped": "not base64!", "salt": "x", "verifier": "y" }),
            "wrapped must be base64",
        ),
        (
            serde_json::json!({ "wrapped": "AAAA", "salt": "AAAA", "verifier": "AAAA" }),
            "wrapped must decode to 60 bytes",
        ),
    ] {
        let response = env
            .with_token(&mac.token, Method::PUT, "/auth/keys")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"],
            message
        );
    }
    let (_, material) = obsink_core::create_account_key("x", &mac.user_id).unwrap();
    let short_salt = env
        .with_token(&mac.token, Method::PUT, "/auth/keys")
        .json(&serde_json::json!({
            "wrapped": material.wrapped_b64(), "salt": "AAAA", "verifier": material.verifier_b64()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(short_salt.status(), 400);
    assert!(env
        .with_token(&mac.token, Method::GET, "/auth/keys")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["account_key"]
        .is_null());
    env.finish().await;
}
