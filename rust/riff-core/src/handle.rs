//! Driving a session from outside its event pump.
//!
//! [`RiffSession::run`] borrows the session for as long as the conversation lasts, which is the
//! right shape for the engine — one owner, no interior mutability, no locks held across an await —
//! and the wrong shape for an embedder, who has to feed the microphone and answer a stop button
//! while it runs.
//!
//! [`RiffHandle`] is that seam. Audio goes straight to the connection, because it touches no session
//! state and must never wait; everything that does touch state is queued and applied by the pump
//! between events, so the session keeps its single owner.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use crate::provider::RealtimeConnection;

/// Something a handle asked for that only the pump can carry out.
pub(crate) enum Command {
    /// Typed input, to be recorded and sent.
    SendText(String),
    /// Cut the agent off.
    Interrupt,
    /// End the session.
    Stop(String),
}

/// The live connection, shared so a handle keeps working across a reconnect.
#[derive(Default)]
pub(crate) struct ConnectionSlot {
    current: Mutex<Option<Arc<dyn RealtimeConnection>>>,
}

impl ConnectionSlot {
    fn lock(&self) -> MutexGuard<'_, Option<Arc<dyn RealtimeConnection>>> {
        self.current
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// The connection, if the session has one.
    pub(crate) fn get(&self) -> Option<Arc<dyn RealtimeConnection>> {
        self.lock().clone()
    }

    /// Replaces what is in the slot, handing back what was there.
    pub(crate) fn replace(
        &self,
        connection: Option<Arc<dyn RealtimeConnection>>,
    ) -> Option<Arc<dyn RealtimeConnection>> {
        std::mem::replace(&mut self.lock(), connection)
    }
}

/// Commands waiting for the pump.
#[derive(Default)]
pub(crate) struct CommandQueue {
    state: Mutex<QueueState>,
}

#[derive(Default)]
struct QueueState {
    pending: VecDeque<Command>,
    waker: Option<Waker>,
}

impl CommandQueue {
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Throws away anything queued but not yet carried out.
    ///
    /// A command belongs to the connection it was queued against. Carrying one over to the next
    /// would let a stale `SendText` reach a new session's ledger — speech nobody uttered in that
    /// conversation — and a stale `Stop` close a connection the moment it opened.
    pub(crate) fn drain(&self) {
        self.lock().pending.clear();
    }

    fn push(&self, command: Command) {
        let waker = {
            let mut state = self.lock();
            state.pending.push_back(command);
            state.waker.take()
        };
        // Woken outside the lock: the pump takes it the moment it polls.
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// The next command, once there is one.
    pub(crate) fn next(&self) -> NextCommand<'_> {
        NextCommand(self)
    }
}

/// The future [`CommandQueue::next`] returns.
pub(crate) struct NextCommand<'a>(&'a CommandQueue);

impl Future for NextCommand<'_> {
    type Output = Command;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Command> {
        let mut state = self.0.lock();
        match state.pending.pop_front() {
            Some(command) => Poll::Ready(command),
            None => {
                state.waker = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// Drives a session while its pump is running.
///
/// Cloneable and `Send`, so the audio thread can hold one and the UI another.
///
/// > **Everything except [`send_audio`](RiffHandle::send_audio) is applied by the pump.** A handle
/// > held while nothing is calling [`RiffSession::run`] or [`RiffSession::step`] will queue commands
/// > that are never carried out. The session's own equivalents stay available for an embedder that
/// > drives it by hand instead.
///
/// A handle stays valid across a reconnect, but the commands it queued do not: anything still
/// waiting when a session ends is dropped rather than carried into the next one. A handle is a way
/// to reach the conversation that is happening, not a mailbox for the next.
#[derive(Clone)]
pub struct RiffHandle {
    pub(crate) connection: Arc<ConnectionSlot>,
    pub(crate) commands: Arc<CommandQueue>,
}

impl RiffHandle {
    /// Captured microphone audio, in the format the provider advertised.
    ///
    /// Goes straight to the connection rather than through the queue: it touches no session state,
    /// and audio that waits for a tool call to finish is audio the speaker has to repeat.
    pub fn send_audio(&self, chunk: &[u8]) {
        if let Some(connection) = self.connection.get() {
            connection.send_audio(chunk);
        }
    }

    /// Ends the current turn explicitly. Only needed when turn detection is manual.
    pub fn commit_audio(&self) {
        if let Some(connection) = self.connection.get() {
            connection.commit_audio();
        }
    }

    /// Typed input, treated exactly like speech.
    ///
    /// The recorded [`Utterance`](crate::Utterance) arrives on the event stream rather than being
    /// returned, because the pump is what records it.
    pub fn send_text(&self, text: impl Into<String>) {
        self.commands.push(Command::SendText(text.into()));
    }

    /// Cuts the agent off. Called when the speaker starts talking over it.
    pub fn interrupt(&self) {
        self.commands.push(Command::Interrupt);
    }

    /// Ends the session. The pump stops once it has closed the connection.
    pub fn stop(&self, reason: impl Into<String>) {
        self.commands.push(Command::Stop(reason.into()));
    }
}
