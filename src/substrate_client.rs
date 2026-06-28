use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use thiserror::Error;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::session_store::PersistentSession;
use crate::token_store::{decode_jwt_payload, is_substrate_token_claims};

const SIGNALR_SEP: char = '\x1e';
const WS_BASE: &str = "wss://substrate.office.com/m365Copilot/Chathub";

const VARIANTS: &str = "EnableMcpServerWidgets,feature.EnableMcpServerWidgets,feature.EnableLuForChatCIQ,\
feature.enableChatCIQPlugin,EnableRequestPlugins,feature.EnableSensitivityLabels,\
EnableUnsupportedUrlDetector,feature.IsCustomEngineCopilotEnabled,feature.bizchatfluxv3,\
feature.enablechatpages,feature.enableCodeCanvas,feature.turnOnWorkTabRecommendation,\
feature.turnOnDARecommendation,feature.IsStreamingModeInChatRequestEnabled,\
IncludeSourceAttributionsConcise,SkipPublishEmptyMessage,\
feature.EnableDeduplicatingSourceAttributions,Enable3PActionProgressMessages,\
feature.enableClientWebRtc,feature.EnableMeetingRecapOfSeriesMeetingWithCiq,\
feature.EnableReferencesListCompleteSignal,feature.StorageMessageSplitDisabled,\
feature.EnableCuaTakeControlApi,SingletonEnvOn,feature.cwcallowedos,\
feature.EnableMergingPureDeltas,feature.disabledisallowedmsgs,\
feature.enableCitationsForSynthesisData,feature.EnableConversationShareApis,\
feature.enableGenerateGraphicArtOptionsSet,cdximagen,\
feature.EnableUpdatedUXForConfirmationDialog,\
feature.EnableContentApiandDocTypeHtmlInRichAnswers,\
cdxgrounding_api_v2_rich_web_answers_reference_bottom_force,\
cdxenablerenderforisocomp,feature.EnableClientFileURLSupportForOfficeWebPaidCopilot,\
feature.EnableDesignEditorImageGrounding,feature.EnableDesignerEditor,\
feature.EnableSkipRehydrationForSpeCIdImages,feature.EnableSkipEmittingMessageOnFlush,\
feature.EnableRemoveEmptySourceAttributions,feature.EnableRemoveStreamingMode,\
feature.OfficeWebToHelix,feature.OfficeDesktopToHelix,feature.M365TeamsHubToHelix,\
feature.OwaHubToHelix,feature.MonarchHubToHelix,feature.Win32OutlookHubToHelix,\
feature.MacOutlookHubToHelix,Agt_bizchat_enableGpt5ForHelix";

const OPTIONS_SETS: &[&str] = &[
    "search_result_progress_messages_with_search_queries",
    "cwc_flux_image",
    "cwc_code_interpreter",
    "cwc_code_interpreter_amsfix",
    "cwcfluxgptv",
    "flux_v3_gptv_enable_upload_multi_image_in_turn_wo_ch",
    "cwc_code_interpreter_citation_fix",
    "code_interpreter_interactive_charts",
    "cwc_code_interpreter_interactive_charts_inline_image",
    "code_interpreter_matplotlib_patching",
    "cwc_fileupload_odb",
    "update_memory_plugin",
    "add_custom_instructions",
    "cwc_flux_v3",
    "flux_v3_progress_messages",
    "enable_batch_token_processing",
    "enable_gg_gpt",
    "flux_v3_image_gen_enable_dimensions",
    "flux_v3_image_gen_enable_icon_dimensions",
    "flux_v3_image_gen_enable_system_text_with_params",
    "flux_v3_image_gen_enable_designer_dimensions_meta_prompting_in_system_prompts",
];

const ALLOWED_MESSAGE_TYPES: &[&str] = &[
    "Chat", "Suggestion", "InternalSearchQuery", "Disengaged",
    "InternalLoaderMessage", "Progress", "GeneratedCode", "RenderCardRequest",
    "AdsQuery", "SemanticSerp", "GenerateContentQuery", "GenerateGraphicArt",
    "SearchQuery", "ConfirmationCard", "AuthError", "DeveloperLogs",
    "TriggerPlugin", "HintInvocation", "MemoryUpdate", "EndOfRequest",
    "TriggerConfirmation", "ResumeInvokeAction", "ResumeUserInputRequest",
    "TriggerUserInputRequest", "EscapeHatch", "TriggerPluginAuth",
    "ResumePluginAuth", "SideBySide", "ReferencesListComplete",
    "SwitchRespondingEndpoint",
];

#[derive(Debug, Error)]
#[error("{0}")]
pub struct SubstrateCopilotError(pub String);

#[derive(Clone)]
pub struct SubstrateCopilotClient {
    token: String,
    time_zone: String,
    oid: String,
    tid: String,
}

impl SubstrateCopilotClient {
    pub fn new(access_token: &str, time_zone: &str) -> Result<Self, SubstrateCopilotError> {
        if access_token.is_empty() {
            return Err(SubstrateCopilotError(
                "M365_ACCESS_TOKEN is missing. Start the debug Edge window and let startup token capture complete, \
                 or run `copilot-openai-proxy set-token`."
                    .into(),
            ));
        }

        let claims = decode_jwt_payload(access_token)
            .map_err(|e| SubstrateCopilotError(format!("Cannot decode access token: {e}")))?;

        if !is_substrate_token_claims(&claims) {
            return Err(SubstrateCopilotError(
                "Access token is not a substrate.office.com token.".into(),
            ));
        }

        let exp = claims.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        if now > exp {
            return Err(SubstrateCopilotError(
                "Access token expired. To refresh: open M365 Copilot in your browser, \
                 DevTools → Network → filter 'substrate' → click the WebSocket → Headers → \
                 copy the access_token= query param → update M365_ACCESS_TOKEN in .env"
                    .into(),
            ));
        }

        let oid = claims
            .get("oid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SubstrateCopilotError("missing oid claim".into()))?
            .to_string();
        let tid = claims
            .get("tid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SubstrateCopilotError("missing tid claim".into()))?
            .to_string();

        Ok(Self {
            token: access_token.to_string(),
            time_zone: time_zone.to_string(),
            oid,
            tid,
        })
    }

    fn ws_url(&self, conv_id: &str, session_id: &str, req_id: &str) -> String {
        let token = urlencoding::encode(&self.token);
        format!(
            "{WS_BASE}/{}@{}?ClientRequestId={req_id}&X-SessionId={session_id}\
             &ConversationId={conv_id}&access_token={token}&variants={VARIANTS}\
             &source=officeweb&product=Office&agentHost=Bizchat.FullScreen\
             &licenseType=Starter&agent=web&scenario=OfficeWebIncludedCopilot",
            self.oid, self.tid
        )
    }

    fn chat_invoke(
        &self,
        text: &str,
        conv_id: &str,
        session_id: &str,
        req_id: &str,
        is_start_of_session: bool,
    ) -> String {
        let _ = conv_id;
        let payload = json!({
            "arguments": [{
                "source": "officeweb",
                "clientCorrelationId": req_id,
                "sessionId": session_id,
                "optionsSets": OPTIONS_SETS,
                "streamingMode": "ConciseWithPadding",
                "spokenTextMode": "None",
                "options": {},
                "extraExtensionParameters": {},
                "allowedMessageTypes": ALLOWED_MESSAGE_TYPES,
                "sliceIds": [],
                "threadLevelGptId": {},
                "traceId": req_id,
                "isStartOfSession": is_start_of_session,
                "clientInfo": {
                    "clientPlatform": "mcmcopilot-web",
                    "clientAppName": "Office",
                    "clientEntrypoint": "mcmcopilot-officeweb",
                    "clientSessionId": session_id,
                    "clientAppType": "Web",
                    "deviceOS": "Windows",
                    "deviceType": "Desktop",
                },
                "message": {
                    "author": "user",
                    "inputMethod": "Keyboard",
                    "text": text,
                    "entityAnnotationTypes": ["People", "File", "Event", "Email", "TeamsMessage"],
                    "requestId": req_id,
                    "locationInfo": {"timeZoneOffset": 9, "timeZone": self.time_zone},
                    "locale": "en-us",
                    "messageType": "Chat",
                    "experienceType": "Default",
                    "adaptiveCards": [],
                    "clientPreferences": {},
                },
                "plugins": [{"Id": "BingWebSearch", "Source": "BuiltIn"}],
                "isSbsSupported": true,
                "tone": "Magic",
                "renderReferencesBehindEOS": true,
            }],
            "invocationId": "0",
            "target": "chat",
            "type": 4,
        });
        format!("{payload}{SIGNALR_SEP}")
    }

    pub async fn chat(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<String, SubstrateCopilotError> {
        let mut chunks = Vec::new();
        let mut stream = self.chat_stream(prompt, additional_context, session).await?;
        while let Some(chunk) = stream.next().await {
            chunks.push(chunk?);
        }
        Ok(chunks.join(""))
    }

    pub async fn chat_stream(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<
        futures_util::stream::BoxStream<'static, Result<String, SubstrateCopilotError>>,
        SubstrateCopilotError,
    > {
        let text = combine_text(prompt, additional_context);
        let client = self.clone();

        if let Some(session) = session {
            let guard = session.lock.clone().lock_owned().await;
            let turn = session.reserve_turn();
            let stream = client
                .chat_stream_for_turn(
                    text,
                    turn.conversation_id,
                    turn.client_session_id,
                    turn.is_start_of_session,
                )
                .await;
            Ok(Box::pin(LockedStream {
                _guard: guard,
                stream,
            }))
        } else {
            Ok(client
                .chat_stream_for_turn(
                    text,
                    Uuid::new_v4().to_string(),
                    Uuid::new_v4().to_string(),
                    true,
                )
                .await)
        }
    }

    async fn chat_stream_for_turn(
        &self,
        text: String,
        conv_id: String,
        session_id: String,
        is_start_of_session: bool,
    ) -> futures_util::stream::BoxStream<'static, Result<String, SubstrateCopilotError>> {
        let req_id = Uuid::new_v4().to_string();
        let url = self.ws_url(&conv_id, &session_id, &req_id);
        let chat_invoke = self.chat_invoke(&text, &conv_id, &session_id, &req_id, is_start_of_session);

        let result = async {
            let mut request = url.into_client_request().map_err(|e| {
                SubstrateCopilotError(format!("invalid websocket request: {e}"))
            })?;
            request
                .headers_mut()
                .insert("Origin", "https://m365.cloud.microsoft".parse().unwrap());

            let (mut ws, _) = connect_async(request)
                .await
                .map_err(|e| SubstrateCopilotError(e.to_string()))?;

            ws.send(Message::Text(
                format!("{{\"protocol\":\"json\",\"version\":1}}{SIGNALR_SEP}").into(),
            ))
            .await
            .map_err(|e| SubstrateCopilotError(e.to_string()))?;
            let _ = ws.next().await;

            ws.send(Message::Text(chat_invoke.into()))
            .await
            .map_err(|e| SubstrateCopilotError(e.to_string()))?;

            let mut fallback_text = String::new();
            let mut yielded_any = false;
            let mut out = Vec::new();

            while let Some(msg) = ws.next().await {
                let raw = msg.map_err(|e| SubstrateCopilotError(e.to_string()))?;
                let text = match raw {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };

                for part in text.split(SIGNALR_SEP) {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let msg: Value = match serde_json::from_str(part) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let t = msg.get("type").and_then(|v| v.as_i64());
                    if t == Some(6) {
                        continue;
                    }

                    if t == Some(1) && msg.get("target").and_then(|v| v.as_str()) == Some("update")
                    {
                        let args = msg
                            .get("arguments")
                            .and_then(|a| a.as_array())
                            .and_then(|a| a.first())
                            .cloned()
                            .unwrap_or(json!({}));

                        if let Some(delta) = args.get("writeAtCursor").and_then(|v| v.as_str()) {
                            if !delta.is_empty() {
                                if !yielded_any && !fallback_text.is_empty() {
                                    out.push(Ok(fallback_text.clone()));
                                }
                                yielded_any = true;
                                out.push(Ok(delta.to_string()));
                            }
                        }

                        if let Some(msgs) = args.get("messages") {
                            fallback_text = extract_assistant_text(msgs);
                        }
                    }

                    if t == Some(2) {
                        if let Some(item_msgs) = msg
                            .get("item")
                            .and_then(|i| i.get("messages"))
                        {
                            fallback_text = extract_assistant_text(item_msgs);
                        }
                    }

                    if t == Some(3) {
                        if !yielded_any && !fallback_text.is_empty() {
                            out.push(Ok(fallback_text.clone()));
                        }
                        break;
                    }
                }
            }

            Ok::<_, SubstrateCopilotError>(out)
        }
        .await;

        match result {
            Ok(chunks) => futures_util::stream::iter(chunks).boxed(),
            Err(e) => futures_util::stream::once(async move { Err(e) }).boxed(),
        }
    }
}

fn extract_assistant_text(msgs: &Value) -> String {
    let entries: Vec<&Value> = match msgs {
        Value::Array(arr) => arr.iter().collect(),
        other => vec![other],
    };
    for entry in entries.into_iter().rev() {
        if entry.get("author").and_then(|v| v.as_str()) != Some("user") {
            return entry
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
        }
    }
    String::new()
}

fn combine_text(prompt: &str, context: &[String]) -> String {
    if context.is_empty() {
        prompt.to_string()
    } else {
        format!("{}\n\n---\n\n{prompt}", context.join("\n\n"))
    }
}

struct LockedStream<S> {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    stream: S,
}

impl<S> futures_util::Stream for LockedStream<S>
where
    S: futures_util::Stream<Item = Result<String, SubstrateCopilotError>> + Unpin,
{
    type Item = Result<String, SubstrateCopilotError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.stream).poll_next(cx)
    }
}
