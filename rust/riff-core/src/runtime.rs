//! The seams Riff needs from whatever runtime the embedder brought.
//!
//! Rust has no runtime in its standard library, and the engine takes no third-party dependencies, so
//! time and delays arrive through a trait rather than through a chosen executor. Every async method
//! in this crate is an ordinary future the embedder drives; nothing here spawns tasks on its own.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A boxed future, which is what makes the host and provider traits usable behind `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Wall clock, monotonic time, and delays.
///
/// An embedder already running on an async runtime should implement this over that runtime's timer;
/// [`SystemClock`] is a working default for anyone who is not.
pub trait Clock: Send + Sync {
    /// The current time, as `YYYY-MM-DDTHH:MM:SS.sssZ`. Every binding writes timestamps this way, so
    /// an artifact captured on one is readable by a consumer written against another.
    fn now(&self) -> String;

    /// Milliseconds since some fixed point, used for durations rather than for timestamps.
    fn monotonic_ms(&self) -> u64;

    /// Resolves after `milliseconds`. This is what bounds a tool call, so a host that never returns
    /// cannot hold a turn open forever.
    fn sleep(&self, milliseconds: u64) -> BoxFuture<'static, ()>;
}

/// The default clock: the system time, and a thread per delay.
///
/// A thread per delay is fine for the handful of timers a session uses, and it is the only way to
/// wait without a runtime. An embedder that already has one should implement [`Clock`] over it.
#[derive(Debug)]
pub struct SystemClock {
    started: Instant,
}

impl SystemClock {
    /// A clock anchored at the moment it is created.
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        format_rfc3339(millis)
    }

    fn monotonic_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn sleep(&self, milliseconds: u64) -> BoxFuture<'static, ()> {
        Box::pin(ThreadSleep {
            milliseconds,
            state: None,
        })
    }
}

/// Formats milliseconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.sssZ`.
pub fn format_rfc3339(millis_since_epoch: u128) -> String {
    let total_seconds = (millis_since_epoch / 1000) as i64;
    let millis = (millis_since_epoch % 1000) as u32;

    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    )
}

/// Days since the Unix epoch to a civil date, by the usual era-based conversion.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;

    (year + i64::from(month <= 2), month, day)
}

struct ThreadSleep {
    milliseconds: u64,
    state: Option<Arc<Mutex<SleepState>>>,
}

#[derive(Default)]
struct SleepState {
    elapsed: bool,
    waker: Option<Waker>,
}

impl Future for ThreadSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();

        let state = this.state.get_or_insert_with(|| {
            let state = Arc::new(Mutex::new(SleepState::default()));
            let timer = Arc::clone(&state);
            let milliseconds = this.milliseconds;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(milliseconds));
                let waker = {
                    let mut timer = timer.lock().unwrap_or_else(|error| error.into_inner());
                    timer.elapsed = true;
                    timer.waker.take()
                };
                if let Some(waker) = waker {
                    waker.wake();
                }
            });
            state
        });

        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        if state.elapsed {
            Poll::Ready(())
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Whichever of two futures finished first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Either<A, B> {
    /// The first future finished.
    Left(A),
    /// The second future finished.
    Right(B),
}

/// Polls two futures and yields whichever finishes first, preferring the first on a tie.
///
/// Both futures must be [`Unpin`], which every [`BoxFuture`] is. Wrap an `async` block in
/// [`Box::pin`] to race one.
pub fn race<A, B>(first: A, second: B) -> Race<A, B> {
    Race { first, second }
}

/// The future [`race`] returns.
#[derive(Debug)]
pub struct Race<A, B> {
    first: A,
    second: B,
}

impl<A: Future + Unpin, B: Future + Unpin> Future for Race<A, B> {
    type Output = Either<A::Output, B::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Poll::Ready(value) = Pin::new(&mut this.first).poll(cx) {
            return Poll::Ready(Either::Left(value));
        }
        if let Poll::Ready(value) = Pin::new(&mut this.second).poll(cx) {
            return Poll::Ready(Either::Right(value));
        }
        Poll::Pending
    }
}

/// Runs an operation with a deadline, returning `None` when the deadline arrives first.
///
/// A host that never returns would otherwise hold the whole batch open, and the single continuation
/// the model is waiting for would never be requested — the conversation just stops. A timeout is a
/// result the model can act on; silence is not.
///
/// The abandoned operation is dropped rather than waited on, which in Rust cancels it at its next
/// suspension point. A host doing work that has a side effect the speaker can see must still make it
/// idempotent: the send may already be in flight when the caller is told it failed.
pub async fn with_deadline<T>(
    milliseconds: u64,
    clock: &dyn Clock,
    operation: BoxFuture<'_, T>,
) -> Option<T> {
    if milliseconds == 0 {
        return Some(operation.await);
    }
    match race(operation, clock.sleep(milliseconds)).await {
        Either::Left(value) => Some(value),
        Either::Right(()) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps_the_way_every_binding_writes_them() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00.000Z");
        // 2026-01-02T03:04:05.006Z
        assert_eq!(
            format_rfc3339(1_767_323_045_006),
            "2026-01-02T03:04:05.006Z"
        );
    }

    #[test]
    fn handles_a_leap_day() {
        // 2024-02-29T12:00:00.000Z
        assert_eq!(
            format_rfc3339(1_709_208_000_000),
            "2024-02-29T12:00:00.000Z"
        );
    }
}
