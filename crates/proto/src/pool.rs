//! The policy of a connection pool, without I/O: which connection to lend,
//! when to open or close one, who waits and in what order. Whoever drives
//! it (`rustmy.Pool`, `rustmy.aio.create_pool`) holds the connections,
//! opens and closes them, and does the waiting; this keeps the accounts, by
//! connection id, with the time the caller gives.
//!
//! Every connection is counted from the moment it is to be opened, so the
//! pool never goes past its maximum while connections are being opened.
//! Idle connections are lent most recently used first, so the rest age and
//! expire; whoever waits is served in order.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

/// A pool's limits.
#[derive(Debug, Clone, Copy, Default)]
pub struct PoolConfig {
    /// Connections kept open, lent or idle (`minsize`).
    pub min: usize,
    /// Connections open at once, at most (`maxsize`).
    pub max: usize,
    /// A connection this old is closed instead of lent again
    /// (`pool_recycle`).
    pub max_lifetime: Option<Duration>,
    /// An idle connection beyond the minimum is closed once unused this
    /// long.
    pub idle_timeout: Option<Duration>,
    /// A connection idle this long is pinged before it is lent.
    pub pre_ping: Option<Duration>,
}

/// A connection, as the pool knows it.
pub type ConnId = u64;
/// Someone waiting for a connection.
pub type Ticket = u64;

/// What [`PoolCore::acquire`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquire {
    /// Take the idle connection `id`, pinging it first if `ping`.
    Lend { id: ConnId, ping: bool },
    /// Open a connection and report it with [`PoolCore::opened`] (or
    /// [`PoolCore::open_failed`]).
    Open,
    /// Wait for `ticket` to be granted ([`Effects::grants`]) or failed
    /// ([`Effects::failed`]).
    Wait(Ticket),
    /// The pool is closed.
    Closed,
}

/// What someone waiting gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// The connection `id`, just given back.
    Lend { ticket: Ticket, id: ConnId },
    /// Room for a connection: open it, as after [`Acquire::Open`].
    Open { ticket: Ticket },
}

/// What the caller has to do after a call.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Connections to close; the pool no longer counts them.
    pub close: Vec<ConnId>,
    /// Waiters that may go on.
    pub grants: Vec<Grant>,
    /// Connections to open for the minimum, each reported with
    /// [`PoolCore::idle_opened`] (or [`PoolCore::open_failed`]).
    pub open: usize,
    /// Waiters to fail: the pool closed.
    pub failed: Vec<Ticket>,
}

#[derive(Debug, Clone, Copy)]
struct Idle {
    id: ConnId,
    created: Instant,
    since: Instant,
}

/// A pool's accounts ([`crate::pool`]).
#[derive(Debug)]
pub struct PoolCore {
    config: PoolConfig,
    /// Most recently given back last.
    idle: Vec<Idle>,
    /// Lent connections and when each was opened.
    lent: HashMap<ConnId, Instant>,
    /// Connections being opened.
    opening: usize,
    waiting: VecDeque<Ticket>,
    next: u64,
    closed: bool,
}

impl PoolCore {
    /// A pool with no connections yet; [`PoolCore::start`] opens the
    /// minimum.
    ///
    /// # Errors
    ///
    /// The limits make no sense: no room for a connection, or a minimum
    /// above the maximum.
    pub fn new(config: PoolConfig) -> Result<Self, &'static str> {
        if config.max == 0 {
            return Err("maxsize must be at least 1");
        }
        if config.min > config.max {
            return Err("minsize must not be larger than maxsize");
        }
        Ok(Self {
            config,
            idle: Vec::new(),
            lent: HashMap::new(),
            opening: 0,
            waiting: VecDeque::new(),
            next: 1,
            closed: false,
        })
    }

    /// The connections to open first, for the minimum.
    pub fn start(&mut self) -> Effects {
        let mut effects = Effects::default();
        self.rebalance(&mut effects);
        effects
    }

    /// Someone wants a connection.
    pub fn acquire(&mut self, now: Instant) -> (Acquire, Effects) {
        let mut effects = Effects::default();
        if self.closed {
            return (Acquire::Closed, effects);
        }
        self.expire(now, &mut effects);
        let acquire = if !self.waiting.is_empty() {
            // Nobody jumps the queue.
            self.wait()
        } else if let Some(conn) = self.idle.pop() {
            self.lent.insert(conn.id, conn.created);
            let ping = self
                .config
                .pre_ping
                .is_some_and(|after| now.saturating_duration_since(conn.since) >= after);
            Acquire::Lend { id: conn.id, ping }
        } else if self.size() < self.config.max {
            self.opening += 1;
            Acquire::Open
        } else {
            self.wait()
        };
        self.rebalance(&mut effects);
        (acquire, effects)
    }

    /// The connection [`Acquire::Open`] or [`Grant::Open`] asked for is
    /// open, and lent to whoever opened it: its id.
    pub fn opened(&mut self, now: Instant) -> ConnId {
        self.opening = self.opening.saturating_sub(1);
        let id = self.next_id();
        self.lent.insert(id, now);
        id
    }

    /// A connection [`Effects::open`] asked for is open: it goes to the
    /// first waiter, or idle.
    pub fn idle_opened(&mut self, now: Instant) -> (ConnId, Effects) {
        let mut effects = Effects::default();
        self.opening = self.opening.saturating_sub(1);
        let id = self.next_id();
        if self.closed {
            effects.close.push(id);
        } else if let Some(ticket) = self.waiting.pop_front() {
            self.lent.insert(id, now);
            effects.grants.push(Grant::Lend { ticket, id });
        } else {
            self.idle.push(Idle {
                id,
                created: now,
                since: now,
            });
        }
        (id, effects)
    }

    /// A connection could not be opened: its room goes to the first waiter
    /// (who tries in turn) or to the minimum.
    pub fn open_failed(&mut self) -> Effects {
        let mut effects = Effects::default();
        self.opening = self.opening.saturating_sub(1);
        self.rebalance(&mut effects);
        effects
    }

    /// The lent connection `id` comes back: lent again, idle, or closed
    /// (when `reuse` is false, it is too old, or the pool closed). An id
    /// the pool does not lend changes nothing.
    pub fn release(&mut self, id: ConnId, now: Instant, reuse: bool) -> Effects {
        let mut effects = Effects::default();
        let Some(created) = self.lent.remove(&id) else {
            return effects;
        };
        let too_old = self
            .config
            .max_lifetime
            .is_some_and(|limit| now.saturating_duration_since(created) >= limit);
        if !reuse || too_old || self.closed {
            effects.close.push(id);
        } else if let Some(ticket) = self.waiting.pop_front() {
            self.lent.insert(id, created);
            effects.grants.push(Grant::Lend { ticket, id });
        } else {
            self.idle.push(Idle {
                id,
                created,
                since: now,
            });
        }
        self.expire(now, &mut effects);
        self.rebalance(&mut effects);
        effects
    }

    /// `ticket` stops waiting (its timeout, or its task cancelled): false
    /// when it was granted or failed already, and the caller then has to
    /// take what it got.
    pub fn cancel(&mut self, ticket: Ticket) -> bool {
        let before = self.waiting.len();
        self.waiting.retain(|&waiting| waiting != ticket);
        self.waiting.len() != before
    }

    /// Closes the pool: the idle connections now, the lent ones as they
    /// come back; nobody waits any more and nobody gets a connection.
    pub fn close(&mut self) -> Effects {
        self.closed = true;
        Effects {
            close: self.idle.drain(..).map(|conn| conn.id).collect(),
            failed: self.waiting.drain(..).collect(),
            ..Effects::default()
        }
    }

    /// Closes the idle connections (opening new ones for the minimum).
    pub fn clear(&mut self) -> Effects {
        let mut effects = Effects {
            close: self.idle.drain(..).map(|conn| conn.id).collect(),
            ..Effects::default()
        };
        self.rebalance(&mut effects);
        effects
    }

    /// Closes idle connections that are too old, or unused for too long
    /// beyond the minimum (and opens others for the minimum).
    pub fn expire_idle(&mut self, now: Instant) -> Effects {
        let mut effects = Effects::default();
        self.expire(now, &mut effects);
        self.rebalance(&mut effects);
        effects
    }

    /// Connections counted: lent, idle and being opened.
    #[must_use]
    pub fn size(&self) -> usize {
        self.idle.len() + self.lent.len() + self.opening
    }

    #[must_use]
    pub const fn idle_count(&self) -> usize {
        self.idle.len()
    }

    #[must_use]
    pub fn lent_count(&self) -> usize {
        self.lent.len()
    }

    #[must_use]
    pub const fn opening_count(&self) -> usize {
        self.opening
    }

    #[must_use]
    pub fn waiting_count(&self) -> usize {
        self.waiting.len()
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Closed, with every connection given back and none being opened.
    #[must_use]
    pub fn is_drained(&self) -> bool {
        self.closed && self.lent.is_empty() && self.opening == 0
    }

    #[must_use]
    pub const fn config(&self) -> &PoolConfig {
        &self.config
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }

    fn wait(&mut self) -> Acquire {
        let ticket = self.next_id();
        self.waiting.push_back(ticket);
        Acquire::Wait(ticket)
    }

    fn expire(&mut self, now: Instant, effects: &mut Effects) {
        let PoolConfig {
            min,
            max_lifetime,
            idle_timeout,
            ..
        } = self.config;
        if max_lifetime.is_none() && idle_timeout.is_none() {
            return;
        }
        let mut size = self.size();
        let close = &mut effects.close;
        // Least recently used first, so those go before the minimum.
        self.idle.retain(|conn| {
            let too_old = max_lifetime
                .is_some_and(|limit| now.saturating_duration_since(conn.created) >= limit);
            let unused = size > min
                && idle_timeout
                    .is_some_and(|limit| now.saturating_duration_since(conn.since) >= limit);
            if too_old || unused {
                close.push(conn.id);
                size -= 1;
                false
            } else {
                true
            }
        });
    }

    /// Room goes to whoever waits; then, with nobody waiting, connections
    /// are opened up to the minimum.
    fn rebalance(&mut self, effects: &mut Effects) {
        while self.size() < self.config.max
            && let Some(ticket) = self.waiting.pop_front()
        {
            self.opening += 1;
            effects.grants.push(Grant::Open { ticket });
        }
        if self.closed {
            return;
        }
        while self.waiting.is_empty() && self.size() < self.config.min {
            self.opening += 1;
            effects.open += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn config(min: usize, max: usize) -> PoolConfig {
        PoolConfig {
            min,
            max,
            ..PoolConfig::default()
        }
    }

    #[test]
    fn limits_are_checked() {
        assert!(PoolCore::new(config(0, 0)).is_err());
        assert!(PoolCore::new(config(3, 2)).is_err());
        assert!(PoolCore::new(config(2, 2)).is_ok());
    }

    #[test]
    fn opens_the_minimum_then_on_demand_up_to_the_maximum() {
        let now = Instant::now();
        let mut pool = PoolCore::new(config(1, 2)).unwrap();
        assert_eq!(pool.start().open, 1);
        let (first, effects) = pool.idle_opened(now);
        assert_eq!(effects, Effects::default());
        assert_eq!(
            pool.acquire(now).0,
            Acquire::Lend {
                id: first,
                ping: false
            }
        );
        assert_eq!(pool.acquire(now).0, Acquire::Open);
        let second = pool.opened(now);
        let Acquire::Wait(ticket) = pool.acquire(now).0 else {
            panic!("should wait");
        };
        assert_eq!(pool.size(), 2);
        // Given back, the connection goes to whoever waits.
        let effects = pool.release(second, now, true);
        assert_eq!(effects.grants, [Grant::Lend { ticket, id: second }]);
        // Closed instead, its room does.
        let Acquire::Wait(next) = pool.acquire(now).0 else {
            panic!("should wait");
        };
        let effects = pool.release(first, now, false);
        assert_eq!(effects.close, [first]);
        assert_eq!(effects.grants, [Grant::Open { ticket: next }]);
        assert_eq!(pool.size(), 2);
        // Below the minimum with nobody waiting, one is opened.
        let third = pool.opened(now);
        pool.release(second, now, false);
        let effects = pool.release(third, now, false);
        assert_eq!(effects.open, 1);
    }

    #[test]
    fn idle_connections_are_lent_most_recently_used_first() {
        let now = Instant::now();
        let mut pool = PoolCore::new(config(0, 3)).unwrap();
        let ids: Vec<_> = (0..3)
            .map(|_| {
                assert_eq!(pool.acquire(now).0, Acquire::Open);
                pool.opened(now)
            })
            .collect();
        for &id in &ids {
            assert_eq!(pool.release(id, now, true), Effects::default());
        }
        let lent = |pool: &mut PoolCore| match pool.acquire(now).0 {
            Acquire::Lend { id, .. } => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(lent(&mut pool), ids[2]);
        assert_eq!(lent(&mut pool), ids[1]);
    }

    #[test]
    fn pings_connections_idle_for_long() {
        let now = Instant::now();
        let mut pool = PoolCore::new(PoolConfig {
            pre_ping: Some(Duration::from_secs(30)),
            ..config(0, 1)
        })
        .unwrap();
        pool.acquire(now);
        let id = pool.opened(now);
        pool.release(id, now, true);
        assert_eq!(
            pool.acquire(now + Duration::from_secs(10)).0,
            Acquire::Lend { id, ping: false }
        );
        pool.release(id, now + Duration::from_secs(10), true);
        assert_eq!(
            pool.acquire(now + Duration::from_secs(40)).0,
            Acquire::Lend { id, ping: true }
        );
    }

    #[test]
    fn old_and_unused_connections_are_closed() {
        let now = Instant::now();
        let mut pool = PoolCore::new(PoolConfig {
            max_lifetime: Some(Duration::from_secs(100)),
            idle_timeout: Some(Duration::from_secs(10)),
            ..config(1, 3)
        })
        .unwrap();
        pool.start();
        let (kept, _) = pool.idle_opened(now);
        assert_eq!(
            pool.acquire(now).0,
            Acquire::Lend {
                id: kept,
                ping: false
            }
        );
        assert_eq!(pool.acquire(now).0, Acquire::Open);
        assert_eq!(pool.acquire(now).0, Acquire::Open);
        let (a, b) = (pool.opened(now), pool.opened(now));
        // Not before the timeout, and the minimum stays.
        for id in [kept, a, b] {
            pool.release(id, now, true);
        }
        assert_eq!(pool.idle_count(), 3);
        let effects = pool.expire_idle(now + Duration::from_secs(11));
        assert_eq!(effects.close, [kept, a]);
        assert_eq!(pool.idle_count(), 1);
        // Too old: closed when given back, and the minimum reopened.
        assert_eq!(
            pool.acquire(now + Duration::from_secs(11)).0,
            Acquire::Lend { id: b, ping: false }
        );
        let effects = pool.release(b, now + Duration::from_secs(100), true);
        assert_eq!(effects.close, [b]);
        assert_eq!(effects.open, 1);
    }

    #[test]
    fn a_failed_open_gives_its_room_to_whoever_waits() {
        let now = Instant::now();
        let mut pool = PoolCore::new(config(0, 1)).unwrap();
        assert_eq!(pool.acquire(now).0, Acquire::Open);
        let Acquire::Wait(ticket) = pool.acquire(now).0 else {
            panic!("should wait");
        };
        assert_eq!(pool.open_failed().grants, [Grant::Open { ticket }]);
        assert_eq!(pool.size(), 1);
    }

    #[test]
    fn closing_fails_the_waiters_and_closes_connections_as_they_come_back() {
        let now = Instant::now();
        let mut pool = PoolCore::new(config(0, 1)).unwrap();
        pool.acquire(now);
        let id = pool.opened(now);
        let Acquire::Wait(ticket) = pool.acquire(now).0 else {
            panic!("should wait");
        };
        let effects = pool.close();
        assert_eq!(effects.failed, [ticket]);
        assert_eq!(pool.acquire(now).0, Acquire::Closed);
        assert!(!pool.is_drained());
        assert_eq!(pool.release(id, now, true).close, [id]);
        assert!(pool.is_drained());
    }

    #[test]
    fn a_cancelled_ticket_is_not_served() {
        let now = Instant::now();
        let mut pool = PoolCore::new(config(0, 1)).unwrap();
        pool.acquire(now);
        let id = pool.opened(now);
        let Acquire::Wait(ticket) = pool.acquire(now).0 else {
            panic!("should wait");
        };
        assert!(pool.cancel(ticket));
        assert!(!pool.cancel(ticket));
        assert_eq!(pool.release(id, now, true).grants, []);
        assert_eq!(pool.idle_count(), 1);
    }

    /// xorshift64*: random runs without a dependency, reproducible from
    /// the seed.
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let value = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32;
            usize::try_from(value).unwrap() % n.max(1)
        }

        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }
    }

    /// The caller's side of a run: what it holds and what it waits for.
    #[derive(Default)]
    struct Model {
        holding: Vec<ConnId>,
        waiting: HashSet<Ticket>,
        /// Opens whose connection goes to the opener, and opens for the
        /// minimum.
        opens: usize,
        idle_opens: usize,
        created: HashSet<ConnId>,
        closed: HashSet<ConnId>,
    }

    impl Model {
        fn apply(&mut self, effects: Effects) {
            for id in effects.close {
                assert!(self.created.contains(&id), "closes {id}, never opened");
                assert!(self.closed.insert(id), "closes {id} twice");
                assert!(!self.holding.contains(&id), "closes {id}, still lent");
            }
            for grant in effects.grants {
                match grant {
                    Grant::Lend { ticket, id } => {
                        assert!(self.waiting.remove(&ticket), "grants {ticket} twice");
                        assert!(!self.holding.contains(&id), "lends {id} twice");
                        self.holding.push(id);
                    }
                    Grant::Open { ticket } => {
                        assert!(self.waiting.remove(&ticket), "grants {ticket} twice");
                        self.opens += 1;
                    }
                }
            }
            self.idle_opens += effects.open;
            for ticket in effects.failed {
                assert!(self.waiting.remove(&ticket), "fails {ticket}, not waiting");
            }
        }

        fn check(&self, pool: &PoolCore) {
            let config = pool.config();
            assert!(pool.size() <= config.max, "past the maximum");
            assert_eq!(pool.lent_count(), self.holding.len());
            assert_eq!(pool.opening_count(), self.opens + self.idle_opens);
            assert_eq!(pool.waiting_count(), self.waiting.len());
            if pool.idle_count() > 0 {
                assert!(self.waiting.is_empty(), "waits with idle connections");
            }
            if !self.waiting.is_empty() {
                assert_eq!(pool.size(), config.max, "waits with room");
            }
            if !pool.is_closed() {
                assert!(pool.size() >= config.min, "under the minimum");
            }
        }
    }

    fn run(seed: u64) {
        let mut rng = Rng(seed);
        let duration = |rng: &mut Rng| {
            rng.chance(50)
                .then(|| Duration::from_secs(u64::try_from(rng.below(20) + 1).unwrap()))
        };
        let max = rng.below(4) + 1;
        let config = PoolConfig {
            min: rng.below(max + 1),
            max,
            max_lifetime: duration(&mut rng),
            idle_timeout: duration(&mut rng),
            pre_ping: duration(&mut rng),
        };
        let mut pool = PoolCore::new(config).unwrap();
        let mut model = Model::default();
        let mut now = Instant::now();
        model.apply(pool.start());
        for step in 0..3000 {
            match rng.below(9) {
                0 | 1 => match pool.acquire(now) {
                    (Acquire::Lend { id, .. }, effects) => {
                        model.apply(effects);
                        model.holding.push(id);
                    }
                    (Acquire::Open, effects) => {
                        model.apply(effects);
                        model.opens += 1;
                    }
                    (Acquire::Wait(ticket), effects) => {
                        model.waiting.insert(ticket);
                        model.apply(effects);
                    }
                    (Acquire::Closed, effects) => model.apply(effects),
                },
                2 if model.opens > 0 => {
                    model.opens -= 1;
                    if rng.chance(80) {
                        let id = pool.opened(now);
                        assert!(model.created.insert(id));
                        model.holding.push(id);
                    } else {
                        model.apply(pool.open_failed());
                    }
                }
                3 if model.idle_opens > 0 => {
                    model.idle_opens -= 1;
                    if rng.chance(80) {
                        let (id, effects) = pool.idle_opened(now);
                        assert!(model.created.insert(id));
                        model.apply(effects);
                    } else {
                        model.apply(pool.open_failed());
                    }
                }
                4 | 5 if !model.holding.is_empty() => {
                    let id = model.holding.swap_remove(rng.below(model.holding.len()));
                    model.apply(pool.release(id, now, rng.chance(85)));
                }
                6 if !model.waiting.is_empty() => {
                    let ticket = *model.waiting.iter().next().unwrap();
                    assert!(pool.cancel(ticket));
                    model.waiting.remove(&ticket);
                }
                7 => now += Duration::from_secs(u64::try_from(rng.below(8)).unwrap()),
                8 if rng.chance(10) => {
                    let effects = if rng.chance(30) {
                        pool.clear()
                    } else {
                        pool.expire_idle(now)
                    };
                    model.apply(effects);
                }
                _ if step > 2500 && rng.chance(2) && !pool.is_closed() => {
                    model.apply(pool.close());
                }
                _ => {}
            }
            model.check(&pool);
        }
        // Closing ends with every connection closed exactly once.
        model.apply(pool.close());
        while model.opens > 0 || model.idle_opens > 0 || !model.holding.is_empty() {
            if model.opens > 0 {
                model.opens -= 1;
                let id = pool.opened(now);
                assert!(model.created.insert(id));
                model.holding.push(id);
            } else if model.idle_opens > 0 {
                model.idle_opens -= 1;
                let (id, effects) = pool.idle_opened(now);
                assert!(model.created.insert(id));
                model.apply(effects);
            } else if let Some(id) = model.holding.pop() {
                model.apply(pool.release(id, now, true));
            }
            model.check(&pool);
        }
        assert!(pool.is_drained());
        assert_eq!(pool.size(), 0);
        assert_eq!(model.created, model.closed, "seed {seed}");
    }

    #[test]
    fn random_runs_keep_the_accounts() {
        for seed in 1..=300_u64 {
            run(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        }
    }
}
