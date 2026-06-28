use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::StreamExt;

use crate::session_store::PersistentSession;
use crate::substrate_client::{SubstrateCopilotClient, SubstrateCopilotError};

pub type CopilotStream =
    futures_util::stream::BoxStream<'static, Result<String, SubstrateCopilotError>>;

#[async_trait]
pub trait CopilotClient: Send + Sync {
    async fn chat(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<String, SubstrateCopilotError>;

    async fn chat_stream(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<CopilotStream, SubstrateCopilotError>;
}

#[async_trait]
impl CopilotClient for SubstrateCopilotClient {
    async fn chat(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<String, SubstrateCopilotError> {
        SubstrateCopilotClient::chat(self, prompt, additional_context, session).await
    }

    async fn chat_stream(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<CopilotStream, SubstrateCopilotError> {
        SubstrateCopilotClient::chat_stream(self, prompt, additional_context, session).await
    }
}

pub type FakeCopilotCall = (String, Vec<String>, Option<String>);

/// In-memory copilot client for HTTP integration tests.
pub struct FakeCopilotClient {
    pub reply: String,
    pub stream_chunks: Vec<String>,
    pub fail_stream: bool,
    pub calls: std::sync::Mutex<Vec<FakeCopilotCall>>,
}

impl FakeCopilotClient {
    pub fn simple(reply: &str) -> Self {
        Self {
            reply: reply.into(),
            stream_chunks: vec![reply.into()],
            fail_stream: false,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn streaming(chunks: &[&str]) -> Self {
        Self {
            reply: chunks.join(""),
            stream_chunks: chunks.iter().map(|s| s.to_string()).collect(),
            fail_stream: false,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn failing_stream() -> Self {
        Self {
            reply: String::new(),
            stream_chunks: vec![],
            fail_stream: true,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn record(&self, prompt: &str, ctx: &[String], session: Option<Arc<PersistentSession>>) {
        let key = session.as_ref().map(|s| s.conversation_id.clone());
        self.calls
            .lock()
            .unwrap()
            .push((prompt.to_string(), ctx.to_vec(), key));
    }
}

#[async_trait]
impl CopilotClient for FakeCopilotClient {
    async fn chat(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<String, SubstrateCopilotError> {
        self.record(prompt, additional_context, session);
        Ok(self.reply.clone())
    }

    async fn chat_stream(
        &self,
        prompt: &str,
        additional_context: &[String],
        session: Option<Arc<PersistentSession>>,
    ) -> Result<CopilotStream, SubstrateCopilotError> {
        self.record(prompt, additional_context, session);
        if self.fail_stream {
            return Err(SubstrateCopilotError("upstream broke".into()));
        }
        Ok(futures_util::stream::iter(
            self.stream_chunks
                .iter()
                .cloned()
                .map(Ok::<_, SubstrateCopilotError>)
                .collect::<Vec<_>>(),
        )
        .boxed())
    }
}
