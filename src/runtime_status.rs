use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServicePhase {
    #[default]
    Ready,
    WaitingForEdge,
    CapturingToken,
    CaptureFailed,
}

impl ServicePhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::WaitingForEdge => "waiting for Edge M365 tab",
            Self::CapturingToken => "capturing token — click Copilot box, type 1 char",
            Self::CaptureFailed => "token capture failed — run set-token",
        }
    }
}

#[derive(Clone, Default)]
pub struct RuntimeStatus {
    phase: Arc<RwLock<ServicePhase>>,
}

impl RuntimeStatus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_phase(&self, phase: ServicePhase) {
        *self.phase.write().unwrap() = phase;
    }

    pub fn phase(&self) -> ServicePhase {
        *self.phase.read().unwrap()
    }
}
