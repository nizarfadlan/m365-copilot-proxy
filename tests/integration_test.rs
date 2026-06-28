use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use m365_copilot_proxy::cdp::{find_m365_page, needs_substrate_token};
use m365_copilot_proxy::config::Settings;
use m365_copilot_proxy::routes::{create_router, default_app_state_simple};
use m365_copilot_proxy::session_store::PersistentSessionStore;
use m365_copilot_proxy::substrate_client::SubstrateCopilotClient;
use m365_copilot_proxy::token_store::{
    decode_jwt_payload, is_substrate_token, read_env_token, seconds_remaining, write_token,
    AccessTokenStore,
};
use serde_json::{json, Value};
use tower::ServiceExt;

fn make_jwt(exp: i64, aud: &str) -> String {
    fn encode(data: &Value) -> String {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
        URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(data).unwrap())
            .trim_end_matches('=')
            .to_string()
    }
    let header = json!({"alg": "none"});
    let payload = json!({"aud": aud, "exp": exp, "oid": "oid", "tid": "tid"});
    format!("{}.{}.sig", encode(&header), encode(&payload))
}

#[tokio::test]
async fn models_endpoint() {
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let app = create_router(default_app_state_simple(settings));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["data"][0]["id"], "m365-copilot");
}

#[tokio::test]
async fn healthz_includes_token_status() {
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let app = create_router(default_app_state_simple(settings));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["token"]["valid"], true);
}

#[test]
fn token_status_reports_expiry() {
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let store = AccessTokenStore::new(token, ".env");
    let status = store.status();
    assert!(status.valid);
    assert!(status.expires_at.is_some());
    assert!(status.seconds_remaining > 0);
}

#[test]
fn rejects_non_substrate_token() {
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "394866fc-eedb",
    );
    let store = AccessTokenStore::new(token.clone(), ".env");
    let status = store.status();
    assert!(!status.valid);
    assert_eq!(
        status.error.as_deref(),
        Some("Access token is not a substrate.office.com token.")
    );
    match SubstrateCopilotClient::new(&token, "Asia/Tokyo") {
        Err(err) => assert!(err.to_string().contains("not a substrate.office.com token")),
        Ok(_) => panic!("expected non-substrate token to be rejected"),
    }
}

#[test]
fn env_token_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let env_path = dir.path().join(".env");
    std::fs::write(&env_path, "# M365_ACCESS_TOKEN=old\nOTHER=value\n").unwrap();
    write_token(&env_path, &token).unwrap();
    assert_eq!(read_env_token(&env_path).as_deref(), Some(token.as_str()));
    assert_eq!(
        std::fs::read_to_string(&env_path)
            .unwrap()
            .matches("M365_ACCESS_TOKEN=")
            .count(),
        2
    );
}

#[test]
fn cli_helpers() {
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let remaining = seconds_remaining(&token).unwrap();
    assert!(remaining > 0 && remaining <= 3600);
    assert!(is_substrate_token(&token));
    assert!(!is_substrate_token(&make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "394866fc-eedb",
    )));
    assert!(needs_substrate_token(None));
    assert!(needs_substrate_token(Some(&make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - 1,
        "https://substrate.office.com/sydney",
    ))));
    assert!(!needs_substrate_token(Some(&token)));
}

#[test]
fn finds_real_m365_page_not_devtools() {
    let tabs = vec![
        json!({
            "type": "page",
            "url": "devtools://devtools/bundled/devtools_app.html?remoteBase=https://m365.cloud.microsoft/chat",
        }),
        json!({
            "type": "page",
            "url": "https://m365.cloud.microsoft/chat",
        }),
    ];
    let found = find_m365_page(&tabs).unwrap();
    assert_eq!(
        found.get("url").and_then(|v| v.as_str()),
        Some("https://m365.cloud.microsoft/chat")
    );
}

#[test]
fn persistent_session_turn_flags() {
    let session = PersistentSessionStore::default().get("work");
    let first = session.reserve_turn();
    let second = session.reserve_turn();
    assert_eq!(first.conversation_id, second.conversation_id);
    assert_eq!(first.client_session_id, second.client_session_id);
    assert!(first.is_start_of_session);
    assert!(!second.is_start_of_session);
}

#[test]
fn jwt_decode_works() {
    let token = make_jwt(1234567890, "https://substrate.office.com/sydney");
    let claims = decode_jwt_payload(&token).unwrap();
    assert_eq!(claims.get("exp").and_then(|v| v.as_i64()), Some(1234567890));
}

#[tokio::test]
async fn responses_requires_final_user_message() {
    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let app = create_router(default_app_state_simple(settings));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "ignored",
                        "input": [
                            {"role": "user", "content": "Hello"},
                            {"role": "assistant", "content": "Hi"},
                        ],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["detail"],
        "The final Responses input message must be a user message."
    );
}

#[tokio::test]
async fn chat_completion_with_fake_client() {
    use std::sync::Arc;

    use m365_copilot_proxy::copilot::FakeCopilotClient;
    use m365_copilot_proxy::routes::app_state_with_client;

    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let fake = Arc::new(FakeCopilotClient::simple("copilot reply"));
    let app = create_router(app_state_with_client(settings, fake.clone()));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "m365-copilot",
                        "messages": [
                            {"role": "system", "content": "Be concise."},
                            {"role": "user", "content": "Hello"},
                        ],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["choices"][0]["message"]["content"], "copilot reply");
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn persistent_session_header_reuses_conversation() {
    use std::sync::Arc;

    use m365_copilot_proxy::copilot::FakeCopilotClient;
    use m365_copilot_proxy::routes::app_state_with_client;

    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let fake = Arc::new(FakeCopilotClient::simple("ok"));
    let app = create_router(app_state_with_client(settings, fake.clone()));
    let body = json!({
        "model": "m365-copilot",
        "messages": [{"role": "user", "content": "Hello"}],
    })
    .to_string();

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("X-M365-Session-Id", "work")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].2, calls[1].2);
    assert!(calls[0].2.is_some());
}

#[tokio::test]
async fn openai_streaming_returns_sse_chunks() {
    use std::sync::Arc;

    use m365_copilot_proxy::copilot::FakeCopilotClient;
    use m365_copilot_proxy::routes::app_state_with_client;

    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let fake = Arc::new(FakeCopilotClient::streaming(&["hello", " world"]));
    let app = create_router(app_state_with_client(settings, fake));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "m365-copilot",
                        "stream": true,
                        "messages": [{"role": "user", "content": "Hello"}],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("hello"));
    assert!(body.contains(" world"));
    assert!(body.contains("data: [DONE]"));
}

#[tokio::test]
async fn streaming_upstream_error_in_sse() {
    use std::sync::Arc;

    use m365_copilot_proxy::copilot::FakeCopilotClient;
    use m365_copilot_proxy::routes::app_state_with_client;

    let token = make_jwt(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600,
        "https://substrate.office.com/sydney",
    );
    let settings = Settings {
        access_token: token,
        ..Settings::default()
    };
    let fake = Arc::new(FakeCopilotClient::failing_stream());
    let app = create_router(app_state_with_client(settings, fake));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "m365-copilot",
                        "stream": true,
                        "messages": [{"role": "user", "content": "Hello"}],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("upstream_error"));
    assert!(body.contains("data: [DONE]"));
}
