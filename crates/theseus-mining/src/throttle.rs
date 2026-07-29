//! Mining process commands.

/// Commands the supervisor thread accepts.
#[derive(Debug, Clone)]
pub enum MinerCommand {
    Start,
    Stop,
    YieldForChat(String),
    ResumeAfterChat,
    ResetDebounce,
}
