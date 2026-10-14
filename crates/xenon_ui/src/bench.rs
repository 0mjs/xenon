//! A repeatable rendering benchmark, for performance work:
//!
//! ```sh
//! XENON_BENCH="select * from big_table" xenon path/to/file.db
//! ```
//!
//! Once the query's results are on screen, Xenon sits idle (editor focused,
//! cursor blinking) and then scrolls the grid a few rows every frame, measuring
//! the process's CPU use in each phase, then prints the numbers and quits. The
//! window has to stay visible (macOS doesn't draw hidden windows), so only run
//! it when nobody else is using the machine.

use std::time::{Duration, Instant};

pub const PHASE: Duration = Duration::from_secs(8);
/// Rows scrolled per frame in the scrolling phase.
pub const ROWS_PER_FRAME: usize = 3;

pub fn sql() -> Option<String> {
    std::env::var("XENON_BENCH")
        .ok()
        .filter(|sql| !sql.trim().is_empty())
}

/// User + system CPU time used by this process so far.
pub fn cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage fills the struct it's given; RUSAGE_SELF is always valid.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let time = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    time(usage.ru_utime) + time(usage.ru_stime)
}

#[derive(Clone, Copy)]
pub struct Mark {
    wall: Instant,
    cpu: Duration,
}

impl Mark {
    pub fn now() -> Self {
        Self {
            wall: Instant::now(),
            cpu: cpu_time(),
        }
    }

    /// CPU use since this mark, as a percentage of one core.
    pub fn cpu_percent(&self) -> f64 {
        let wall = self.wall.elapsed().as_secs_f64();
        (cpu_time() - self.cpu).as_secs_f64() / wall * 100.
    }

    pub fn elapsed(&self) -> Duration {
        self.wall.elapsed()
    }
}

pub enum Phase {
    /// Waiting for the connection and the query.
    Loading,
    Idle(Mark),
    Scrolling {
        mark: Mark,
        frames: u32,
        row: usize,
        idle_cpu: f64,
    },
    Done,
}

pub struct Bench {
    pub sql: String,
    pub phase: Phase,
}
