use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::Level;
use uuid::Uuid;

use crate::config::Settings;
use crate::copilot::CopilotClient;
use crate::models::{
    AnthropicMessagesRequest, OpenAIChatRequest, OpenAIResponsesRequest, TranslatedRequest,
};
use crate::session_store::{PersistentSession, PersistentSessionStore};
use crate::substrate_client::{SubstrateCopilotClient, SubstrateCopilotError};
use crate::token_store::AccessTokenStore;
use crate::translator::{
    translate_anthropic_request, translate_openai_request, translate_responses_request,
};

const PERSIST_MODEL_SUFFIX: &str = ":persist";
const SESSION_ID_HEADER: &str = "x-m365-session-id";

pub type CopilotClientFactory =
    Arc<dyn Fn() -> Result<Arc<dyn CopilotClient>, SubstrateCopilotError> + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub token_store: Arc<AccessTokenStore>,
    pub session_store: PersistentSessionStore,
    pub copilot_client_factory: CopilotClientFactory,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/token/status", get(token_status))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/responses", post(openai_responses))
        .route("/v1/messages", post(anthropic_messages))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<_>| {
                    tracing::span!(
                        Level::INFO,
                        "http_request",
                        method = %req.method(),
                        uri = %req.uri(),
                    )
                })
                .on_response(
                    |response: &axum::http::Response<_>,
                     latency: std::time::Duration,
                     _span: &tracing::Span| {
                        tracing::info!(
                            status = %response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "request completed"
                        );
                    },
                ),
        )
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "token": state.token_store.status().to_json(),
    }))
}

async fn token_status(State(state): State<AppState>) -> Json<Value> {
    Json(state.token_store.status().to_json())
}

async fn list_models(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": [{
            "id": state.settings.model_alias,
            "object": "model",
            "owned_by": "microsoft-365-copilot",
        }],
    }))
}

async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<OpenAIChatRequest>,
) -> Result<Response, AppError> {
    let translated = translate_openai_request(&request).map_err(AppError::bad_request)?;
    let session = persistent_session(&state, &headers, &request.model, request.user.as_deref());

    let client = (state.copilot_client_factory)()?;

    if request.stream {
        return Ok(sse_response(openai_stream(
            state.settings.model_alias.clone(),
            client,
            translated,
            session,
        )));
    }

    let text = client
        .chat(&translated.prompt, &translated.additional_context, session)
        .await?;

    Ok(Json(chat_completion_json(&state.settings.model_alias, &text)).into_response())
}

async fn openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, AppError> {
    let request: OpenAIResponsesRequest =
        serde_json::from_value(body).map_err(|e| AppError::bad_request(e.to_string()))?;
    let translated = translate_responses_request(&request).map_err(AppError::bad_request)?;
    let session = persistent_session(&state, &headers, &request.model, None);
    let client = (state.copilot_client_factory)()?;

    if request.stream {
        return Ok(sse_response(responses_stream(
            state.settings.model_alias.clone(),
            client,
            translated,
            session,
        )));
    }

    let text = client
        .chat(&translated.prompt, &translated.additional_context, session)
        .await?;

    let created = unix_now();
    Ok(Json(json!({
        "id": format!("resp_{}", Uuid::new_v4().simple()),
        "object": "response",
        "created_at": created,
        "model": state.settings.model_alias,
        "output": [{
            "type": "message",
            "id": format!("msg_{}", Uuid::new_v4().simple()),
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        }],
        "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0},
    }))
    .into_response())
}

async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AnthropicMessagesRequest>,
) -> Result<Response, AppError> {
    let translated = translate_anthropic_request(&request).map_err(AppError::bad_request)?;
    let session = persistent_session(&state, &headers, &request.model, None);
    let client = (state.copilot_client_factory)()?;

    if request.stream {
        return Ok(sse_response(anthropic_stream(
            state.settings.model_alias.clone(),
            client,
            translated,
            session,
        )));
    }

    let text = client
        .chat(&translated.prompt, &translated.additional_context, session)
        .await?;

    Ok(Json(json!({
        "id": format!("msg_{}", Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": state.settings.model_alias,
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 0, "output_tokens": 0},
    }))
    .into_response())
}

fn persistent_session(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
    fallback_key: Option<&str>,
) -> Option<Arc<PersistentSession>> {
    if let Some(header_key) = headers
        .get(SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(state.session_store.get(&format!("header:{header_key}")));
    }
    if model.ends_with(PERSIST_MODEL_SUFFIX) {
        let key = fallback_key.unwrap_or("default");
        return Some(state.session_store.get(&format!("model:{key}")));
    }
    None
}

fn chat_completion_json(model_alias: &str, text: &str) -> Value {
    json!({
        "id": format!("chatcmpl_{}", Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": unix_now(),
        "model": model_alias,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop",
        }],
    })
}

fn sse_response(
    stream: impl futures_util::Stream<Item = Result<String, SubstrateCopilotError>> + Send + 'static,
) -> Response {
    let body = Body::from_stream(stream.map(|item| {
        item.map(axum::body::Bytes::from)
            .map_err(|e| std::io::Error::other(e.to_string()))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}

fn openai_stream(
    model_alias: String,
    client: Arc<dyn CopilotClient>,
    translated: TranslatedRequest,
    session: Option<Arc<PersistentSession>>,
) -> impl futures_util::Stream<Item = Result<String, SubstrateCopilotError>> + Send + 'static {
    async_stream::stream! {
        let completion_id = format!("chatcmpl_{}", Uuid::new_v4().simple());
        let created = unix_now();
        let first = json!({
            "id": completion_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model_alias,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}],
        });
        yield Ok(format!("data: {first}\n\n"));

        match client.chat_stream(&translated.prompt, &translated.additional_context, session).await {
            Ok(mut upstream) => {
                while let Some(delta) = upstream.next().await {
                    match delta {
                        Ok(content) => {
                            let chunk = json!({
                                "id": completion_id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model_alias,
                                "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}],
                            });
                            yield Ok(format!("data: {chunk}\n\n"));
                        }
                        Err(e) => {
                            let err = json!({"error": {"message": e.to_string(), "type": "upstream_error"}});
                            yield Ok(format!("data: {err}\n\n"));
                            yield Ok("data: [DONE]\n\n".into());
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                let err = json!({"error": {"message": e.to_string(), "type": "upstream_error"}});
                yield Ok(format!("data: {err}\n\n"));
                yield Ok("data: [DONE]\n\n".into());
                return;
            }
        }

        let final_chunk = json!({
            "id": completion_id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model_alias,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        });
        yield Ok(format!("data: {final_chunk}\n\n"));
        yield Ok("data: [DONE]\n\n".into());
    }
}

fn responses_stream(
    model_alias: String,
    client: Arc<dyn CopilotClient>,
    translated: TranslatedRequest,
    session: Option<Arc<PersistentSession>>,
) -> impl futures_util::Stream<Item = Result<String, SubstrateCopilotError>> + Send + 'static {
    async_stream::stream! {
        let resp_id = format!("resp_{}", Uuid::new_v4().simple());
        let item_id = format!("msg_{}", Uuid::new_v4().simple());
        let created = unix_now();

        yield Ok(format!("data: {}\n\n", json!({
            "type": "response.created",
            "response": {"id": resp_id, "object": "response", "created_at": created, "model": model_alias, "status": "in_progress", "output": []},
        })));
        yield Ok(format!("data: {}\n\n", json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"id": item_id, "type": "message", "role": "assistant", "content": []},
        })));
        yield Ok(format!("data: {}\n\n", json!({
            "type": "response.content_part.added",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {"type": "output_text", "text": ""},
        })));

        let mut full_text = String::new();
        match client.chat_stream(&translated.prompt, &translated.additional_context, session).await {
            Ok(mut upstream) => {
                while let Some(delta) = upstream.next().await {
                    match delta {
                        Ok(content) => {
                            full_text.push_str(&content);
                            yield Ok(format!("data: {}\n\n", json!({
                                "type": "response.output_text.delta",
                                "item_id": item_id,
                                "output_index": 0,
                                "content_index": 0,
                                "delta": content,
                            })));
                        }
                        Err(e) => {
                            yield Ok(format!("data: {}\n\n", json!({
                                "type": "error",
                                "error": {"message": e.to_string(), "type": "upstream_error"},
                            })));
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                yield Ok(format!("data: {}\n\n", json!({
                    "type": "error",
                    "error": {"message": e.to_string(), "type": "upstream_error"},
                })));
                return;
            }
        }

        yield Ok(format!("data: {}\n\n", json!({
            "type": "response.output_text.done",
            "item_id": item_id,
            "output_index": 0,
            "content_index": 0,
            "text": full_text,
        })));
        yield Ok(format!("data: {}\n\n", json!({
            "type": "response.completed",
            "response": {
                "id": resp_id,
                "object": "response",
                "created_at": created,
                "model": model_alias,
                "status": "completed",
                "output": [{
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": full_text}],
                }],
                "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0},
            },
        })));
    }
}

fn anthropic_stream(
    model_alias: String,
    client: Arc<dyn CopilotClient>,
    translated: TranslatedRequest,
    session: Option<Arc<PersistentSession>>,
) -> impl futures_util::Stream<Item = Result<String, SubstrateCopilotError>> + Send + 'static {
    async_stream::stream! {
        let msg_id = format!("msg_{}", Uuid::new_v4().simple());

        yield Ok(sse_event("message_start", json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model_alias,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        })));
        yield Ok(sse_event("content_block_start", json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""},
        })));
        yield Ok(sse_event("ping", json!({"type": "ping"})));

        match client.chat_stream(&translated.prompt, &translated.additional_context, session).await {
            Ok(mut upstream) => {
                while let Some(delta) = upstream.next().await {
                    match delta {
                        Ok(content) => {
                            yield Ok(sse_event("content_block_delta", json!({
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": {"type": "text_delta", "text": content},
                            })));
                        }
                        Err(e) => {
                            yield Ok(sse_event("error", json!({
                                "type": "error",
                                "error": {"type": "upstream_error", "message": e.to_string()},
                            })));
                            return;
                        }
                    }
                }
            }
            Err(e) => {
                yield Ok(sse_event("error", json!({
                    "type": "error",
                    "error": {"type": "upstream_error", "message": e.to_string()},
                })));
                return;
            }
        }

        yield Ok(sse_event("content_block_stop", json!({"type": "content_block_stop", "index": 0})));
        yield Ok(sse_event("message_delta", json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 0},
        })));
        yield Ok(sse_event("message_stop", json!({"type": "message_stop"})));
    }
}

fn sse_event(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

pub struct AppError {
    status: StatusCode,
    detail: String,
}

impl AppError {
    fn bad_request(detail: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            detail,
        }
    }
}

impl From<SubstrateCopilotError> for AppError {
    fn from(err: SubstrateCopilotError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            detail: err.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"detail": self.detail}))).into_response()
    }
}

pub fn default_app_state(
    settings: Settings,
    _env_file: PathBuf,
    token_store: Arc<AccessTokenStore>,
) -> AppState {
    let settings_clone = settings.clone();
    let token_store_clone = token_store.clone();
    AppState {
        settings,
        token_store,
        session_store: PersistentSessionStore::default(),
        copilot_client_factory: Arc::new(move || {
            let client =
                SubstrateCopilotClient::new(&token_store_clone.get(), &settings_clone.time_zone)?;
            Ok(Arc::new(client) as Arc<dyn CopilotClient>)
        }),
    }
}

/// Build app state with a custom copilot client (integration tests).
pub fn app_state_with_client(settings: Settings, client: Arc<dyn CopilotClient>) -> AppState {
    let token_store = Arc::new(AccessTokenStore::new(settings.access_token.clone(), ".env"));
    AppState {
        settings,
        token_store,
        session_store: PersistentSessionStore::default(),
        copilot_client_factory: Arc::new(move || Ok(client.clone())),
    }
}

/// Convenience for tests.
pub fn default_app_state_simple(settings: Settings) -> AppState {
    let token_store = Arc::new(AccessTokenStore::new(settings.access_token.clone(), ".env"));
    default_app_state(settings, PathBuf::from(".env"), token_store)
}
