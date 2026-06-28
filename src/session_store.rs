use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CopilotTurn {
    pub conversation_id: String,
    pub client_session_id: String,
    pub is_start_of_session: bool,
}

pub struct PersistentSession {
    pub conversation_id: String,
    pub client_session_id: String,
    turn_count: Mutex<u32>,
    pub lock: Arc<AsyncMutex<()>>,
}

impl PersistentSession {
    fn new() -> Self {
        Self {
            conversation_id: Uuid::new_v4().to_string(),
            client_session_id: Uuid::new_v4().to_string(),
            turn_count: Mutex::new(0),
            lock: Arc::new(AsyncMutex::new(())),
        }
    }

    pub fn reserve_turn(&self) -> CopilotTurn {
        let mut count = self.turn_count.lock().unwrap();
        let turn = CopilotTurn {
            conversation_id: self.conversation_id.clone(),
            client_session_id: self.client_session_id.clone(),
            is_start_of_session: *count == 0,
        };
        *count += 1;
        turn
    }
}

#[derive(Clone, Default)]
pub struct PersistentSessionStore {
    sessions: Arc<Mutex<HashMap<String, Arc<PersistentSession>>>>,
}

impl PersistentSessionStore {
    pub fn get(&self, key: &str) -> Arc<PersistentSession> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(PersistentSession::new()))
            .clone()
    }
}
