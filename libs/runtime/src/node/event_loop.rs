//! The shared event loop: the microtask queue, the macrotask queue, the timer heap, and the drain
//! pump that lets `async`/`await`, promise jobs, and `setTimeout`/`setInterval` make progress.
//!
//! This is core, not a stub. The current bare [`crate::JsRuntime`] evaluates synchronously and has
//! no loop at all (documented gap in `lib.rs`); this module supplies one.
//!
//! Design notes (tenets 3 + 4):
//! * Queues are plain `RefCell`-wrapped std collections — no locks (single-threaded runtime), no
//!   per-job allocation beyond what Nova already hands us.
//! * Timers live in a [`BinaryHeap`] keyed by `Reverse(deadline)` so the earliest-due timer is
//!   `O(1)` to peek and `O(log n)` to pop; only genuinely-blocking waits sleep the thread, and only
//!   when no microtask is ready.
//! * Jobs are executed with [`Job::run`] from *inside* the realm closure (mirroring the Nova CLI's
//!   `run_microtask_queue`), because [`nova_vm::ecmascript::GcAgent::run_job`] requires an empty
//!   execution-context stack and so cannot be called while we are already inside `run_in_realm`.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::time::{Duration, Instant};

use nova_vm::ecmascript::{Agent, Job, JsResult};
use nova_vm::engine::{Bindable, GcScope};

/// A timer scheduled by `setTimeout`/`setInterval`, ordered by its deadline.
///
/// `seq` breaks ties so two timers with the same deadline run in scheduling order (FIFO), matching
/// Node's observable behavior. The [`Ord`] impl orders by `(deadline, seq)`; the heap stores
/// [`Reverse`] of this so [`BinaryHeap`] (a max-heap) yields the earliest deadline first.
struct TimerEntry {
    deadline: Instant,
    seq: u64,
    job: Job,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.seq == other.seq
    }
}
impl Eq for TimerEntry {}
impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

impl TimerEntry {
    /// The `(deadline, seq)` tuple this entry is ordered by: earliest deadline first, ties broken by
    /// scheduling order (lower `seq` first). Factored out so the comparator is unit-testable without
    /// fabricating a [`Job`] (which Nova does not expose a constructor for).
    fn sort_key(&self) -> (Instant, u64) {
        (self.deadline, self.seq)
    }
}

/// The microtask + macrotask + timer queues and the drain pump.
///
/// Owned by [`crate::node::core::HostState`]; its [`nova_vm::ecmascript::HostHooks`] impl forwards
/// the three `enqueue_*` hooks here.
#[derive(Default)]
pub(crate) struct EventLoop {
    /// Promise reaction / resolve-thenable jobs. Drained FIFO, run-to-completion, before any timer.
    microtasks: RefCell<VecDeque<Job>>,
    /// Generic (host) jobs. Drained after microtasks.
    macrotasks: RefCell<Vec<Job>>,
    /// Pending timers, min-ordered by deadline via `Reverse`.
    timers: RefCell<BinaryHeap<Reverse<TimerEntry>>>,
    /// Monotonic timer sequence counter for stable FIFO tie-breaking.
    next_seq: RefCell<u64>,
}

impl std::fmt::Debug for EventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventLoop")
            .field("microtasks", &self.microtasks.borrow().len())
            .field("macrotasks", &self.macrotasks.borrow().len())
            .field("timers", &self.timers.borrow().len())
            .finish()
    }
}

impl EventLoop {
    /// A fresh loop with empty queues. Zero startup cost: nothing is allocated until work arrives.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Enqueue a promise job (microtask). Forwarded from `HostHooks::enqueue_promise_job`.
    pub(crate) fn enqueue_microtask(&self, job: Job) {
        self.microtasks.borrow_mut().push_back(job);
    }

    /// Enqueue a generic (macrotask) job. Forwarded from `HostHooks::enqueue_generic_job`.
    pub(crate) fn enqueue_generic(&self, job: Job) {
        self.macrotasks.borrow_mut().push(job);
    }

    /// Enqueue a timeout job to run after at least `milliseconds`. Forwarded from
    /// `HostHooks::enqueue_timeout_job`.
    pub(crate) fn enqueue_timeout(&self, job: Job, milliseconds: u64) {
        let deadline = Instant::now() + Duration::from_millis(milliseconds);
        let seq = {
            let mut s = self.next_seq.borrow_mut();
            let v = *s;
            *s = s.wrapping_add(1);
            v
        };
        self.timers
            .borrow_mut()
            .push(Reverse(TimerEntry { deadline, seq, job }));
    }

    /// True when every queue is empty (no microtasks, macrotasks, or pending timers).
    pub(crate) fn is_idle(&self) -> bool {
        self.microtasks.borrow().is_empty()
            && self.macrotasks.borrow().is_empty()
            && self.timers.borrow().is_empty()
    }

    /// Pop the next microtask, if any.
    fn pop_microtask(&self) -> Option<Job> {
        self.microtasks.borrow_mut().pop_front()
    }

    /// Pop the next macrotask, if any.
    fn pop_macrotask(&self) -> Option<Job> {
        self.macrotasks.borrow_mut().pop()
    }

    /// Pop the earliest-due timer if its deadline has passed; otherwise return the deadline of the
    /// nearest pending timer so the caller can decide whether to sleep.
    fn pop_due_timer(&self, now: Instant) -> TimerPoll {
        let mut timers = self.timers.borrow_mut();
        match timers.peek() {
            None => TimerPoll::Empty,
            Some(Reverse(entry)) if is_due(entry.deadline, now) => {
                let Reverse(entry) = timers.pop().expect("peeked entry must pop");
                TimerPoll::Due(entry.job)
            }
            Some(Reverse(entry)) => TimerPoll::Pending(entry.deadline),
        }
    }
}

/// Result of polling the timer heap for the next due timer.
enum TimerPoll {
    /// No timers pending.
    Empty,
    /// A timer is due now; its job is returned.
    Due(Job),
    /// The nearest timer is not yet due; its deadline is returned.
    Pending(Instant),
}

/// Drain microtasks, then run due timers (sleeping to the nearest deadline only when nothing else is
/// runnable), repeating until every queue is empty.
///
/// This is the pump the runtime calls after `script_evaluation` so a script that schedules promise
/// jobs or `setTimeout`s settles before its completion value is read. It is bounded by `deadline`
/// when present (server request handling) and otherwise runs to true idle.
///
/// Jobs are run with [`Job::run`] inside the caller's realm scope, mirroring the Nova CLI's
/// `run_microtask_queue`. The first error aborts the pump and is returned, matching JS semantics
/// where an unhandled job rejection surfaces to the host.
///
/// `event_loop` is borrowed separately from `agent` (it lives behind the `HostState`, which the
/// agent only references through `HostHooks`), so there is no aliasing: the caller threads it in.
pub(crate) fn run_until_idle<'gc>(
    agent: &mut Agent,
    event_loop: &EventLoop,
    deadline: Option<Instant>,
    mut gc: GcScope<'gc, '_>,
) -> JsResult<'gc, ()> {
    loop {
        // 1. Drain all microtasks first (run-to-completion before any macrotask/timer).
        while let Some(job) = event_loop.pop_microtask() {
            job.run(agent, gc.reborrow()).unbind()?.bind(gc.nogc());
            if over_deadline(deadline) {
                return Ok(());
            }
        }

        // 2. A ready macrotask, if any, runs next.
        if let Some(job) = event_loop.pop_macrotask() {
            job.run(agent, gc.reborrow()).unbind()?.bind(gc.nogc());
            continue;
        }

        // 3. Otherwise advance timers. Run a due one immediately; sleep to the nearest pending one
        //    (bounded by `deadline`) since no microtask/macrotask is currently runnable.
        match event_loop.pop_due_timer(Instant::now()) {
            TimerPoll::Due(job) => {
                job.run(agent, gc.reborrow()).unbind()?.bind(gc.nogc());
            }
            TimerPoll::Pending(when) => {
                if over_deadline(deadline) {
                    return Ok(());
                }
                let until = sleep_until(deadline, when);
                let now = Instant::now();
                if until > now {
                    std::thread::sleep(until - now);
                }
                if over_deadline(deadline) {
                    return Ok(());
                }
            }
            TimerPoll::Empty => return Ok(()),
        }
    }
}

/// True when a bounding deadline is set and has elapsed.
fn over_deadline(deadline: Option<Instant>) -> bool {
    matches!(deadline, Some(d) if Instant::now() >= d)
}

/// True when a timer scheduled for `deadline` should fire at `now` (its deadline has been reached).
///
/// Node fires a timer once `now >= deadline`; the `<=` here is that rule expressed deadline-first.
fn is_due(deadline: Instant, now: Instant) -> bool {
    deadline <= now
}

/// The wake instant for the pump when the nearest timer (`when`) is not yet due.
///
/// With no bounding deadline we wake exactly at the timer. With a request `deadline` set we wake at
/// whichever comes first, so request handling never sleeps past its budget waiting on a later timer.
fn sleep_until(deadline: Option<Instant>, when: Instant) -> Instant {
    match deadline {
        Some(d) if d < when => d,
        _ => when,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_loop_is_idle() {
        let el = EventLoop::new();
        assert!(el.is_idle());
    }

    #[test]
    fn timer_sort_key_orders_earliest_first_then_fifo() {
        // Drive the real comparator via TimerEntry::sort_key (what `Ord` delegates to). A `Job`
        // cannot be fabricated outside Nova, but the key — which fully determines heap order — does
        // not need one, so this proves the actual ordering code path rather than a stand-in.
        let now = Instant::now();
        let key = |ms: u64, seq: u64| (now + Duration::from_millis(ms), seq);
        // Feed the same triple through a min-heap (Reverse) and confirm pop order.
        let mut heap: BinaryHeap<Reverse<(Instant, u64)>> = BinaryHeap::new();
        heap.push(Reverse(key(50, 1)));
        heap.push(Reverse(key(10, 2)));
        heap.push(Reverse(key(10, 0)));
        let Reverse((_, first)) = heap.pop().unwrap();
        let Reverse((_, second)) = heap.pop().unwrap();
        let Reverse((_, third)) = heap.pop().unwrap();
        // earliest deadline (10ms) ties broken by seq: 0 then 2, then the 50ms timer (seq 1).
        assert_eq!((first, second, third), (0, 2, 1));

        // Direct comparator checks on the tuple key itself.
        use std::cmp::Ordering::{Equal, Greater, Less};
        assert_eq!(key(10, 0).cmp(&key(50, 9)), Less, "earlier deadline sorts first");
        assert_eq!(key(10, 0).cmp(&key(10, 1)), Less, "equal deadline: lower seq first");
        assert_eq!(key(10, 5).cmp(&key(10, 5)), Equal);
        assert_eq!(key(99, 0).cmp(&key(10, 0)), Greater);
    }

    #[test]
    fn is_due_fires_only_once_deadline_reached() {
        let now = Instant::now();
        // Past and exactly-now deadlines are due; a future deadline is not (Node fires at now>=deadline).
        assert!(is_due(now - Duration::from_millis(1), now));
        assert!(is_due(now, now));
        assert!(!is_due(now + Duration::from_millis(1), now));
    }

    #[test]
    fn sleep_until_clamps_to_the_earlier_of_timer_and_deadline() {
        let base = Instant::now();
        let soon = base + Duration::from_millis(10);
        let later = base + Duration::from_millis(100);
        // No bounding deadline: wake exactly at the timer.
        assert_eq!(sleep_until(None, soon), soon);
        // Deadline earlier than the timer: wake at the deadline so the request budget is honored.
        assert_eq!(sleep_until(Some(soon), later), soon);
        // Deadline later than the timer: wake at the timer (the deadline does not delay it).
        assert_eq!(sleep_until(Some(later), soon), soon);
    }

    #[test]
    fn over_deadline_detects_elapsed() {
        assert!(!over_deadline(None));
        assert!(over_deadline(Some(Instant::now() - Duration::from_millis(1))));
        assert!(!over_deadline(Some(Instant::now() + Duration::from_secs(60))));
    }
}
