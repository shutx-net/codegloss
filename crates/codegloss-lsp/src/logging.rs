//! The server's log: which lines are worth writing, what one looks like, and
//! where it goes.
//!
//! [`init`] is all of it from the outside - `main` calls that and nothing else.
//! Underneath are three answers to three separate questions, kept apart because
//! different things settle them: [`Logger`] is what `tracing` and the `log`
//! facade both ask for, [`Filter`] reads `CODEGLOSS_LOG` in `RUST_LOG`'s
//! grammar, and [`write_timestamp`] writes ISO 8601.
//!
//! # The line
//!
//! ```text
//! 2026-01-01T09:30:00.123456Z  INFO codegloss_lsp::config: cache is ready path="/…" entries=41
//! ```
//!
//! A timestamp, a space, the level right-aligned in five columns, a space, the
//! target, `": "`, then the fields one space apart - the message first and
//! bare, the rest as `name=value` - then a newline. The message leads whatever
//! else was written because `tracing`'s macros put it there: every arm of
//! `event!` that takes one expands to
//! `{ message = format_args!(..), $($fields)* }`.
//!
//! That is byte for byte what `tracing_subscriber::fmt().with_ansi(false)`
//! wrote before this module replaced it - its `Format<Full, SystemTime>` with
//! `DefaultFields`. Being identical is the whole point: the crate is gone, and
//! nobody reading `zed: open log` should be able to tell.
//!
//! Measured rather than assumed, while there was still something to measure
//! against. A throwaway harness put **61 shapes of event** through a real
//! `tracing_subscriber::fmt` and through [`Logger`] - both writing into the
//! same buffer off the same standing clock - and compared the bytes: **no
//! difference anywhere**. The shapes covered all five levels; targets named,
//! one-segment and empty; no fields, one and many; an explicit `message` field
//! before and after the others; messages empty, formatted, 300 bytes long and
//! in another script; `=`, newlines, tabs, quotes, backslashes and NUL inside
//! both a message and a value; every control character the escaping covers;
//! the `%` and `?` sigils and the `field::display`/`field::debug` helpers;
//! owned strings, raw identifiers, dotted names, `field::Empty`, byte slices,
//! 128-bit integers; and errors with chains zero, one and two deep. The real
//! clock was checked apart from the rest, by bracketing 2,000 of upstream's
//! timestamps between two of [`write_timestamp`]'s. The harness is not in the
//! tree: it needs the dependency this module exists to remove.
//!
//! Two differences are left, and both are deliberate:
//!
//! - **The span scope is not printed.** Upstream keeps a registry and writes
//!   `outer:inner: ` in front of the target; this keeps no spans, so it cannot
//!   and does not. Nothing in the workspace opens one - no `span!`, no
//!   `#[instrument]`, all 53 sites are events - and `Logger::new_span` says
//!   what happens if a dependency does.
//! - **A `Debug` impl that returns an error** leaves half a field on the line
//!   here, where upstream threw the whole line away.
//!
//! # The `log` bridge
//!
//! `tracing` is not the only facade in the tree. Under `--features candle`,
//! ureq, rustls, rustls-platform-verifier and tokenizers write through the
//! `log` crate instead - which is to say the model-pack download does, the one
//! thing here that touches a network and the place `AGENTS.md` expects a
//! TLS-intercepting proxy to turn into an `UnknownIssuer` somebody has to be
//! shown. `tracing_subscriber`'s `init` installed `tracing_log::LogTracer`
//! beside the subscriber, so those records have always come out on these lines;
//! [`init`] goes on putting the same [`Logger`] in both registries.
//!
//! Which of them actually speak was measured on the built binary rather than
//! read off a dependency graph. With `CODEGLOSS_LOG=debug` and
//! `CODEGLOSS_MODEL_URL` aimed at a listener that answers TLS with plain HTTP,
//! `--fetch-model` writes eleven lines from `ureq::run`,
//! `ureq_proto::client`, `ureq::unversioned::resolver`,
//! `ureq::unversioned::transport::tcp`, `ureq::tls::rustls`,
//! `rustls::webpki::anchors`, `rustls::client::hs` and
//! `rustls_platform_verifier::verification::others` before this crate's own
//! `error!` - and stdout stays at 0 bytes, which is the other thing that has to
//! be true. `CODEGLOSS_LOG=warn,ureq=debug` leaves seven of the eleven -
//! `ureq_proto` among them, because a target matches on any byte prefix - and
//! drops the four rustls ones, so the filter reaches across the bridge too.
//!
//! A `log` record is a message and nothing else. `tracing_log` turned one into
//! an event carrying the record's target, module path, file and line as fields
//! named `log.*`, and `DefaultVisitor::record_debug` returned on
//! `name.starts_with("log.")` before writing any of them - after
//! `NormalizeEvent::normalized_metadata` had already put the target back where
//! a target goes. So the line is the ordinary one with nothing after the
//! message, which is what `Logger::log_line` writes.
//!
//! Measured the same way and for the same reason, while there was still
//! something to measure against. A throwaway harness put **39 shapes of
//! record** through `tracing_log::LogTracer` and a real
//! `tracing_subscriber::fmt` and through the bridge - both writing into the
//! same buffer off the same standing clock - and compared the bytes: **no
//! difference anywhere**. The shapes covered all five levels; targets with and
//! without `::`, one character long, spaced, empty, and the literal `"log"`
//! that upstream falls back to; messages empty, formatted from parts, 222
//! bytes long and in another script, carrying `=`, quotes, backslashes, tabs,
//! newlines, a carriage return, NUL, every control character the escaping
//! covers and the C1 block; and `module_path`, `file` and `line` set in every
//! combination, including a line of 0. Upstream's `fmt::Subscriber` was told
//! `with_max_level(TRACE)`: its `DEFAULT_MAX_LEVEL` is INFO, and the two
//! quieter levels would otherwise have been compared against silence. The
//! harness is not in the tree: it needs the dependency this module exists to
//! remove.
//!
//! One divergence is deliberate. **`flush` flushes.** Upstream's
//! `LogTracer::flush` did nothing because it owned no sink - it handed each
//! record to `tracing` and kept nothing. This one owns the sink, and whoever
//! calls `log::logger().flush()` is asking for the bytes to be gone.
//!
//! Two things the `log` side needs that the `tracing` side does not: the
//! filter has to be asked inside `log`'s `log` method, because the `log!`
//! macros never call `enabled`; and the global ceiling has to be set once from
//! [`Filter::max_level`], because those macros check it before they build a
//! record at all. Both are in [`init`] and its impl, and both are sound for the
//! reason `register_callsite` is: the filter is settled there and never
//! changes.
//!
//! # The filter
//!
//! The grammar is `RUST_LOG`'s, cut down to the part anyone documents: a bare
//! level (`debug`), or a comma-separated list of `target=level` with at most
//! one bare level among them (`warn,codegloss_lsp::translation=debug`). Span
//! names, field predicates and regular expressions - the rest of what
//! `EnvFilter` accepts - are gone.
//!
//! Two rules are worth stating outright, because both are easy to guess wrong:
//!
//! - **The most specific directive decides on its own.** A list is not a set
//!   of permissions that are OR-ed together: `EnvFilter` sorts its directives
//!   by target length and asks only the first one that matches
//!   (`DirectiveSet::enabled` takes `directives_for(meta).next()`). Measured
//!   against `EnvFilter` itself, `debug,codegloss_lsp::translation=warn` drops
//!   a `debug` event on `codegloss_lsp::translation`, even though the bare
//!   `debug` would allow it alone. [`Filter::allows`] answers the same.
//! - **A list with no bare level is a whitelist.** `codegloss_lsp=debug` drops
//!   every other crate's events, `error` included.
//!
//! Targets are matched by byte prefix rather than by module segment, which is
//! also `EnvFilter`'s rule: `codegloss_lsp` covers
//! `codegloss_lsp::translation`.
//!
//! The agreement was measured rather than assumed. A throwaway harness ran 23
//! directive strings through a real `EnvFilter` subscriber and through
//! [`Filter`], comparing which of 5 targets times 5 levels each let through -
//! 575 decisions per string - and the two answered the same everywhere;
//! [`Filter::max_level`] likewise matched `EnvFilter`'s `max_level_hint` on all
//! 11 strings tried. The harness is not in the tree: it needs the dependency
//! this module exists to remove.
//!
//! Three things are deliberately not `EnvFilter`'s behaviour, all of them
//! places where `EnvFilter` answers with silence:
//!
//! - A word that is not a level is an error here. `EnvFilter` reads `dbug` as a
//!   target name at `trace`, which matches nothing and, leaving no bare level,
//!   logs nothing at all (measured: zero lines).
//! - An empty value counts as unset. See [`Filter::from_env_value`].
//! - Space around either separator is dropped. `EnvFilter` rejects
//!   `codegloss = debug` outright (measured), and a shell variable someone
//!   spaced out is not a different request.
//!
//! IMPORTANT: [`init`] is the only thing here that reads the environment, and
//! it reads it once. `std::env::set_var` is unsafe in this edition and this
//! crate forbids unsafe code, so a test that had to set `CODEGLOSS_LOG` could
//! not be written at all - and one that could would be setting it for every
//! other test in the process. [`Filter::from_env_value`] takes the value
//! instead, exactly as `model_pack::base_url` does, and that is what leaves
//! everything underneath `init` testable.

use std::fmt;
use std::fmt::Write as _;
use std::io;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span;
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

/// Environment variable that sets how much the server logs.
pub const LOG_VARIABLE: &str = "CODEGLOSS_LOG";

/// Separates one directive from the next.
const DIRECTIVE_SEPARATOR: char = ',';
/// Separates a target from the level it is filtered at.
const LEVEL_SEPARATOR: char = '=';

/// Which targets log, and down to which level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    /// Level for targets no directive names. `None` when no bare level was
    /// written, which is what makes such a list a whitelist.
    default: Option<LevelFilter>,
    /// `(target prefix, level)`. Order does not decide anything: the longest
    /// matching prefix wins, so this is kept as written rather than sorted.
    targets: Vec<(String, LevelFilter)>,
}

/// Why a `CODEGLOSS_LOG` string could not be read.
///
/// It names the one directive that failed rather than the whole string: a
/// person who typed `warn,codeglos_lsp=dbug` needs to be told which half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The directive that did not parse, trimmed as it was read.
    directive: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "`{}` is neither a level nor a `target=level` pair",
            self.directive
        )
    }
}

impl std::error::Error for ParseError {}

impl Filter {
    /// Reads a whole `CODEGLOSS_LOG` string.
    ///
    /// Levels are strict and targets are permissive, because the two
    /// vocabularies differ in kind: the levels are a closed set that
    /// [`LevelFilter::from_str`] already knows, while a target is any string an
    /// `info!` may have been given. So a misspelled level is an error here,
    /// where `EnvFilter` would quietly read it as a target name at `trace` -
    /// which is to say as a filter that matches nothing and, having no bare
    /// level left, logs nothing at all.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut filter = Self {
            default: None,
            targets: Vec::new(),
        };

        for directive in text
            .split(DIRECTIVE_SEPARATOR)
            .map(str::trim)
            .filter(|directive| !directive.is_empty())
        {
            let Some((target, level)) = directive.split_once(LEVEL_SEPARATOR) else {
                // No `=` at all: the whole directive is the level to apply to
                // everything not named.
                filter.default = Some(level_of(directive, directive)?);
                continue;
            };

            let target = target.trim();
            if target.is_empty() {
                return Err(ParseError::in_directive(directive));
            }
            // An empty level is `LevelFilter::from_str`'s to answer, and it
            // says ERROR. `EnvFilter` reads `target=` as TRACE instead, but it
            // gets there by special-casing the empty string before it parses,
            // and that special case is the first line of a second copy of the
            // level table.
            let level = level_of(level.trim(), directive)?;

            match filter.targets.iter_mut().find(|(known, _)| known == target) {
                // The same target twice: the last one written wins, which is
                // what `EnvFilter` does by replacing the equal entry in its
                // sorted set.
                Some(entry) => entry.1 = level,
                None => filter.targets.push((target.to_owned(), level)),
            }
        }

        Ok(filter)
    }

    /// Turns the value of [`LOG_VARIABLE`] into a filter, along with the string
    /// that was thrown away if it could not be read.
    ///
    /// Unreadable input falls back to `info` rather than to silence, and the
    /// complaint is handed back rather than logged: there is no subscriber yet
    /// to log it to. Whoever installs one says so afterwards.
    ///
    /// An empty value counts as unset. `EnvFilter` reads `CODEGLOSS_LOG=` as an
    /// empty directive set and drops every event, errors included, which is the
    /// worst way for the thing we tell people to read when something breaks to
    /// fail. `model_pack::base_url` treats an empty value as unset for the same
    /// reason.
    pub fn from_env_value(value: Option<&str>) -> (Self, Option<String>) {
        let Some(text) = value.map(str::trim).filter(|text| !text.is_empty()) else {
            return (Self::default(), None);
        };

        match Self::parse(text) {
            Ok(filter) => (filter, None),
            // The value as it was set, not as it was trimmed: it is quoted back
            // at whoever typed it.
            Err(_) => (Self::default(), value.map(str::to_owned)),
        }
    }

    /// Whether an event with this `target` and `level` should be written.
    pub fn allows(&self, target: &str, level: &Level) -> bool {
        self.targets
            .iter()
            .filter(|(prefix, _)| target.starts_with(prefix.as_str()))
            // The most specific match, and only it. See the module docs: this
            // is not a disjunction over everything that matches.
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, level)| *level)
            .or(self.default)
            .is_some_and(|allowed| allowed >= *level)
    }

    /// The loudest level any directive allows.
    ///
    /// A hint, not the answer: `tracing` uses it to skip callsites no directive
    /// could ever want, and [`allows`](Self::allows) still decides the rest.
    pub fn max_level(&self) -> LevelFilter {
        self.targets
            .iter()
            .map(|(_, level)| *level)
            .chain(self.default)
            .max()
            // Every directive named a target that nothing matches, or there
            // were no directives: nothing can be logged.
            .unwrap_or(LevelFilter::OFF)
    }
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            default: Some(LevelFilter::INFO),
            targets: Vec::new(),
        }
    }
}

impl ParseError {
    /// The directive is copied because the string it was cut from is the
    /// caller's, and the complaint outlives the call.
    fn in_directive(directive: &str) -> Self {
        Self {
            directive: directive.to_owned(),
        }
    }
}

/// The level `text` spells, blamed on the `directive` it came from.
///
/// The spellings are [`LevelFilter::from_str`]'s rather than a table here, so
/// `off`/`error`/`warn`/`info`/`debug`/`trace` in any case and the numbers 0-5
/// all work, and go on meaning what they mean in `RUST_LOG`.
fn level_of(text: &str, directive: &str) -> Result<LevelFilter, ParseError> {
    LevelFilter::from_str(text).map_err(|_| ParseError::in_directive(directive))
}

/// Seconds in a day. Leap seconds are not one of the things a clock reports.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;
/// Days in the 400-year era the calendar repeats over.
const DAYS_PER_ERA: i64 = 400 * 365 + 97;
/// Days from 0000-03-01, the first day of era 0, to the epoch.
const DAYS_FROM_ERA_ZERO_TO_THE_EPOCH: i64 = 719_468;

/// Appends `at` to `out` as ISO 8601 in UTC, to the microsecond:
/// `2026-01-01T09:30:00.123456Z`.
///
/// This is byte for byte what `tracing_subscriber::fmt::time::SystemTime`
/// writes, which is the `Display` of the `DateTime` in its
/// `fmt/time/datetime.rs` (0.3.23): a `T` between the date and the time, a
/// trailing `Z`, six fractional digits taken as `nanos / 1_000` so that what
/// is below them is cut rather than rounded, and the three cases below for a
/// year that does not fit in four digits. Matching it to the byte is the point
/// of the exercise: replacing the crate that used to write this line is not
/// meant to be visible in the line.
///
/// Keeping a timestamp at all is worth the arithmetic. `DEVELOPERS.md` sends
/// anyone whose server is misbehaving to `zed: open log`, and a log with no
/// time on it cannot be lined up against what the editor was doing.
pub fn write_timestamp(out: &mut String, at: SystemTime) {
    let (seconds, nanos) = since_the_epoch(at);
    // Floor division on both, so that an instant before the epoch is a
    // negative day plus a positive time of day rather than a negative time.
    let (year, month, day) = civil_from_days(seconds.div_euclid(SECONDS_PER_DAY));
    let seconds_of_day = seconds.rem_euclid(SECONDS_PER_DAY);

    // Upstream's three cases. `{:04}` is a minimum width rather than a fixed
    // one, so a fifth digit would appear unannounced above 9999, and a
    // negative year would spend one of its four columns on the sign.
    let _ = if year > 9999 {
        write!(out, "+{year}")
    } else if year < 0 {
        write!(out, "{year:05}")
    } else {
        write!(out, "{year:04}")
    };

    let _ = write!(
        out,
        "-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}Z",
        seconds_of_day / 3600,
        seconds_of_day / 60 % 60,
        seconds_of_day % 60,
        // Microseconds, and the rest of the nanoseconds are dropped rather
        // than rounded: rounding 999_999_999 would carry into a seventh digit.
        nanos / 1_000,
    );
}

/// `at` as a signed count of seconds from the epoch and a nanosecond
/// remainder in `0..1_000_000_000`.
///
/// The pair is what upstream's `From<SystemTime> for DateTime` builds, borrow
/// and all.
fn since_the_epoch(at: SystemTime) -> (i64, u32) {
    match at.duration_since(UNIX_EPOCH) {
        Ok(after) => (whole_seconds(after), after.subsec_nanos()),
        Err(before) => {
            // For an earlier instant the distance is measured the other way
            // round, so the fraction runs forwards while the seconds run
            // backwards. The second it is a fraction of is the one below.
            let before = before.duration();
            match before.subsec_nanos() {
                0 => (-whole_seconds(before), 0),
                nanos => (-whole_seconds(before) - 1, 1_000_000_000 - nanos),
            }
        }
    }
}

/// The whole seconds of `span`, clamped rather than wrapped.
///
/// A `Duration` counts seconds in a `u64` and a date needs them signed, so
/// there is a range no clock reaches and every value in it is a broken one.
/// Upstream `debug_assert!`s the same bound and, past it, casts with `as` -
/// which wraps, turning a clock set absurdly far ahead into a date in the deep
/// past. Clamping at least keeps the sign.
fn whole_seconds(span: Duration) -> i64 {
    i64::try_from(span.as_secs()).unwrap_or(i64::MAX)
}

/// The civil date `days` days after 1970-01-01, as `(year, month, day)`.
///
/// Howard Hinnant's `civil_from_days`, in `i64` throughout. The year is shifted
/// to start in March so that the leap day is the last day of it and no month
/// after it has to be stepped over; a 400-year era then holds exactly
/// [`DAYS_PER_ERA`] days no matter where it starts, which is what turns the
/// calendar into four divisions instead of a walk over months. Upstream
/// arrives at the same answers through musl's `__secs_to_tm`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + DAYS_FROM_ERA_ZERO_TO_THE_EPOCH;
    let era = days.div_euclid(DAYS_PER_ERA);
    // 0..=146_096.
    let day_of_era = days.rem_euclid(DAYS_PER_ERA);
    // 0..=399. The three corrections take back the leap days that the 4-, 100-
    // and 400-year rules respectively do and do not grant.
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    // 0..=365, counted from March 1st.
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // 0..=11, likewise from March. Five consecutive months from March take 153
    // days wherever they start, which is what makes this a division.
    let month_of_year = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_of_year + 2) / 5 + 1;
    let month = if month_of_year < 10 {
        month_of_year + 3
    } else {
        month_of_year - 9
    };

    // January and February are the tail of the shifted year, so they belong to
    // the calendar year after it.
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// The name of the field a `tracing` macro puts its message in.
///
/// It is the one field the line does not label, and it comes first: every arm
/// of `event!` that takes a message expands to
/// `{ message = format_args!(..), $($fields)* }` (`tracing`'s `macros.rs`), so
/// the message leads whatever else was named, wherever it was written.
const MESSAGE_FIELD: &str = "message";

/// What a raw identifier is spelled with. `r#type = 1` is a Rust spelling of a
/// field called `type`, and the line writes the name rather than the spelling.
const RAW_IDENTIFIER: &str = "r#";

/// Room for a short line before the string has to grow. A timestamp, a level
/// and a target are sixty-odd bytes of one before anything has been said.
const LINE_CAPACITY: usize = 128;

/// Writes one line per event and keeps nothing between them.
///
/// The sink is a type parameter so that a test can read the bytes back, not so
/// that the destination is a matter of taste: see [`init`] for why there is
/// only one right answer to that.
pub struct Logger<W> {
    /// Settled by [`init`] and never touched again. That it cannot change is
    /// what lets [`Subscriber::register_callsite`] answer once and for all.
    filter: Filter,
    /// Taken once per line, around a single `write_all`, so that two threads
    /// logging at the same moment interleave whole lines and not halves.
    sink: Mutex<W>,
    /// Injected so that a test can stand the clock still.
    clock: fn() -> SystemTime,
    /// The next span id to hand out. Nothing in this workspace opens a span -
    /// no `span!`, no `#[instrument]`, all 53 sites are events - but a
    /// dependency may, and the contract has to hold when one does.
    next_span: AtomicU64,
}

impl Logger<io::Stderr> {
    /// The logger [`init`] installs.
    pub fn stderr(filter: Filter) -> Self {
        Self::to(filter, io::stderr(), SystemTime::now)
    }
}

impl<W> Logger<W> {
    /// A logger writing somewhere else, on a clock of the caller's choosing.
    ///
    /// Both are arguments for the same reason: a subscriber that formats
    /// `SystemTime::now()` into `io::stderr()` cannot be checked against
    /// anything. With them out here the line is an ordinary value.
    pub fn to(filter: Filter, sink: W, clock: fn() -> SystemTime) -> Self {
        Self {
            filter,
            sink: Mutex::new(sink),
            clock,
            // `span::Id::from_u64` panics on zero, so the count starts past it.
            next_span: AtomicU64::new(1),
        }
    }

    /// Everything a line carries before what was said: the time, the level and
    /// the target.
    ///
    /// Both kinds of record share it because upstream shared it. A `log` record
    /// reached the same formatter through `tracing_log`, which put the record's
    /// own target back on the event it made
    /// (`NormalizeEvent::normalized_metadata`), so the two came out on one
    /// shape of line and go on doing so.
    fn opening(&self, level: &Level, target: &str) -> String {
        let mut out = String::with_capacity(LINE_CAPACITY);

        write_timestamp(&mut out, (self.clock)());
        // `Level`'s own `Display` pads, so asking for five columns puts INFO
        // and WARN one space in and leaves the other three where they are -
        // which is what upstream's table of pre-padded spellings amounts to.
        let _ = write!(out, " {level:>5} {target}: ");

        out
    }

    /// The line [`Subscriber::event`] writes, newline and all.
    fn line(&self, event: &Event<'_>) -> String {
        let metadata = event.metadata();
        let mut out = self.opening(metadata.level(), metadata.target());

        event.record(&mut Fields::new(&mut out));
        out.push('\n');

        out
    }

    /// The line [`log::Log::log`] writes.
    ///
    /// A `log` record is a message and nothing else. `tracing_log` carried the
    /// module path, file and line along as fields named `log.target`,
    /// `log.module_path`, `log.file` and `log.line`, and the formatter dropped
    /// every one of them unwritten - `DefaultVisitor::record_debug` returns on
    /// `name.starts_with("log.")` before it pads or writes anything - so none
    /// of them ever reached a line, and none is collected here.
    fn log_line(&self, record: &log::Record<'_>) -> String {
        let mut out = self.opening(&as_tracing(record.level()), record.target());

        // Bare and escaped, exactly as [`Fields`] writes a message: it is the
        // same `fmt::Arguments` on either path, and `Debug` for one of those is
        // its `Display`.
        let _ = write!(Escaping(&mut out), "{}", record.args());
        out.push('\n');

        out
    }
}

impl<W: io::Write> Logger<W> {
    /// Hands one whole line to the sink.
    ///
    /// The lock is taken here rather than by the callers so that there is one
    /// answer to what a line is: both registries write through it, and two
    /// threads logging at the same moment interleave whole lines and not
    /// halves.
    fn write(&self, line: &str) {
        // A poisoned lock means another thread panicked, not that the sink is
        // broken. Giving up on it would silence the log for the rest of the
        // process, which is precisely when there is something worth reading.
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        // And a sink that will not take the bytes is not worth a panic in a
        // language server: the editor would see the process die over a log.
        let _ = sink.write_all(line.as_bytes());
    }
}

/// The `tracing` level a `log` level means.
///
/// `tracing_log`'s `AsTrace`, which is the mapping the two crates agree on:
/// the five are the same five and neither has a sixth.
fn as_tracing(level: log::Level) -> Level {
    match level {
        log::Level::Error => Level::ERROR,
        log::Level::Warn => Level::WARN,
        log::Level::Info => Level::INFO,
        log::Level::Debug => Level::DEBUG,
        log::Level::Trace => Level::TRACE,
    }
}

/// The `log` ceiling a `tracing` one means, `off` included.
///
/// `tracing_log`'s `AsLog`, and the one conversion `init` needs: `log`'s macros
/// check this global before they build a record at all, so a ceiling that is
/// too low silences records the [`Filter`] would have let through - the same
/// trap as [`Subscriber::max_level_hint`], on the other side of the bridge.
fn as_log(level: LevelFilter) -> log::LevelFilter {
    match level {
        LevelFilter::OFF => log::LevelFilter::Off,
        LevelFilter::ERROR => log::LevelFilter::Error,
        LevelFilter::WARN => log::LevelFilter::Warn,
        LevelFilter::INFO => log::LevelFilter::Info,
        LevelFilter::DEBUG => log::LevelFilter::Debug,
        LevelFilter::TRACE => log::LevelFilter::Trace,
    }
}

impl<W: io::Write + Send + 'static> Subscriber for Logger<W> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.filter.allows(metadata.target(), metadata.level())
    }

    /// Answers permanently, which is sound only because the [`Filter`] is
    /// settled in [`init`] and never changes afterwards.
    ///
    /// `tracing` caches this per callsite for the life of the process, so
    /// `Interest::never()` turns a `debug!` nobody asked for into a load and a
    /// branch: no arguments evaluated, no line built, and `enabled` never asked
    /// again. That is the performance argument for the whole module.
    ///
    /// It is `tracing_core`'s own default written out rather than inherited,
    /// because the permanence is a claim about *this* filter rather than a
    /// default that happens to suit. **A filter that could change after
    /// `init` would have to return [`Interest::sometimes`] here.**
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        if self.enabled(metadata) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    /// The loudest level any directive allows.
    ///
    /// `tracing` drops callsites above it without asking [`enabled`](Self::enabled),
    /// so a hint that is too low silences events the filter would have let
    /// through. That is why [`Filter::max_level`] maximises over the bare level
    /// *and* every target rather than reporting the bare one.
    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.filter.max_level())
    }

    /// A fresh id for each span, and never zero.
    ///
    /// The id only has to be distinct and legal, because nothing reads a span
    /// back: `span::Id::from_u64` panics on zero, and the counter reaches zero
    /// only by wrapping. The fields go the same way - with no registry to hold
    /// them there is nowhere to put them, which is also why the line carries
    /// none of the span scope that `Format<Full>` would print in front of the
    /// target.
    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed).max(1))
    }

    /// Dropped: a span's fields are only ever read back out of a registry, and
    /// there is none.
    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}

    /// Dropped: an edge between two spans is worth nothing without the spans.
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    /// Writes the event as one line.
    fn event(&self, event: &Event<'_>) {
        self.write(&self.line(event));
    }

    /// Dropped, with [`exit`](Self::exit): the line carries no scope, so a
    /// stack of current spans would have nothing to feed. `current_span` keeps
    /// its default, `Current::unknown`, which says exactly that.
    fn enter(&self, _span: &span::Id) {}

    /// Dropped, with [`enter`](Self::enter).
    fn exit(&self, _span: &span::Id) {}
}

/// The same logger again, for the other facade.
///
/// `log` and `tracing` keep separate global registries, and a crate that writes
/// through one knows nothing of the other: under `--features candle`, ureq,
/// rustls, rustls-platform-verifier and tokenizers all speak `log`. [`init`]
/// installs one [`Logger`] in both, so a download failing and a translation
/// failing are the same kind of line, in one order, behind one lock.
impl<W: io::Write + Send + 'static> log::Log for Logger<W> {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.filter
            .allows(metadata.target(), &as_tracing(metadata.level()))
    }

    /// The `log!` macros do not call [`enabled`](log::Log::enabled) - they
    /// check only the ceiling [`init`] set, which knows nothing of targets - so
    /// the filter has to be asked here or `codegloss_lsp=debug` would stop
    /// meaning anything on this side. `LogTracer::log` asks for the same
    /// reason.
    fn log(&self, record: &log::Record<'_>) {
        if log::Log::enabled(self, record.metadata()) {
            self.write(&self.log_line(record));
        }
    }

    /// Upstream's `LogTracer::flush` did nothing, because the sink was not its
    /// to flush - it handed the record to `tracing` and kept nothing. This one
    /// owns the sink, and whoever calls `log::logger().flush()` is asking for
    /// the bytes to be gone before something else happens.
    fn flush(&self) {
        let mut sink = self.sink.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = sink.flush();
    }
}

/// Writes an event's fields into the line: the message bare, the rest as
/// `name=value`.
struct Fields<'a> {
    out: &'a mut String,
    /// Whether anything has been written yet. The separator goes *before* each
    /// field rather than after it, so that the line cannot end in one.
    first: bool,
}

impl<'a> Fields<'a> {
    fn new(out: &'a mut String) -> Self {
        Self { out, first: true }
    }

    /// A space before every field but the first.
    fn separate(&mut self) {
        if self.first {
            self.first = false;
        } else {
            self.out.push(' ');
        }
    }
}

impl Visit for Fields<'_> {
    /// A string is the one value whose `Debug` would be wrong for a message:
    /// quotes and escapes around a sentence a person wrote. Every other field
    /// keeps them, because in `name="a b"` the quotes are where the value
    /// visibly ends.
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == MESSAGE_FIELD {
            self.record_debug(field, &format_args!("{value}"));
        } else {
            self.record_debug(field, &value);
        }
    }

    /// An error is written with the chain under it beside it, under a second
    /// name made from the first. Nothing in this workspace records a
    /// `&dyn Error` - every site writes `%error`, which is a `Display` value -
    /// but a dependency may, and then the chain is the half worth having.
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        match value.source() {
            Some(source) => self.record_debug(
                field,
                &format_args!(
                    "{} {}.sources={}",
                    Escaped(value),
                    field.name(),
                    Sources(source)
                ),
            ),
            None => self.record_debug(field, &format_args!("{}", Escaped(value))),
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.separate();

        let name = field.name();
        // A `Debug` that returns an error leaves half a field on the line here
        // and would have thrown the whole line away upstream. Nothing in a log
        // has ever done it, and the alternative is a second copy of the line
        // to write into.
        let _ = if name == MESSAGE_FIELD {
            // The message is the one part of the line written without
            // `Debug`'s escaping, so it is the only place a control character
            // could reach a terminal intact.
            write!(Escaping(&mut *self.out), "{value:?}")
        } else {
            write!(
                self.out,
                "{}={value:?}",
                name.strip_prefix(RAW_IDENTIFIER).unwrap_or(name)
            )
        };
    }
}

/// Passes text through, spelling out the control characters a terminal reads
/// as commands rather than as text.
///
/// A log line ends up on a terminal, and an escape sequence that reaches one
/// intact can move the cursor, repaint the lines above it or set the window
/// title - which is how a log becomes a way to lie about what the log said.
/// The set is `tracing_subscriber`'s `EscapeGuard`: the C0 codes a sequence
/// starts or ends with, and the whole C1 block.
struct Escaping<'a>(&'a mut dyn fmt::Write);

impl fmt::Write for Escaping<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            match character {
                '\x07' => self.0.write_str("\\x07")?,
                '\x08' => self.0.write_str("\\x08")?,
                '\x0c' => self.0.write_str("\\x0c")?,
                '\x1b' => self.0.write_str("\\x1b")?,
                '\x7f' => self.0.write_str("\\x7f")?,
                '\u{80}'..='\u{9f}' => write!(self.0, "\\u{{{:x}}}", character as u32)?,
                _ => self.0.write_char(character)?,
            }
        }

        Ok(())
    }
}

/// The `Display` of `T` with [`Escaping`] over it. Its `Debug` writes the same
/// text, because `Debug` is what a list builder calls on its entries.
struct Escaped<T>(T);

impl<T: fmt::Display> fmt::Display for Escaped<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(Escaping(formatter), "{}", self.0)
    }
}

impl<T: fmt::Display> fmt::Debug for Escaped<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

/// An error and everything under it, as `[outer, inner, …]`.
struct Sources<'a>(&'a (dyn std::error::Error + 'static));

impl fmt::Display for Sources<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = formatter.debug_list();
        let mut current = Some(self.0);

        while let Some(error) = current {
            list.entry(&Escaped(error));
            current = error.source();
        }

        list.finish()
    }
}

/// Installs the logger, and says so if [`LOG_VARIABLE`] could not be read.
///
/// IMPORTANT: stdout carries the JSON-RPC stream. A single stray byte there
/// corrupts the protocol and the editor kills the server, so every line goes to
/// stderr and this crate must never `println!`.
///
/// The complaint about an unreadable filter comes after the install rather than
/// before it, because before it there is no subscriber to complain to. A second
/// call installs nothing: both registries refuse once they hold a logger, and a
/// server that failed to start over a log line would be the worse failure. It
/// does say so, on the first call's logger, which is the one thing a second
/// call can usefully do.
///
/// The `log` bridge is installed here too, and it is the same [`Logger`]:
/// `tracing_subscriber`'s `init` also installed `tracing_log::LogTracer`, so
/// records written through the `log` facade have always come out on these
/// lines. Nothing in the default build writes one, but under
/// `--features candle` ureq, rustls, rustls-platform-verifier and tokenizers
/// all do - which is to say the model-pack download, the one thing here that
/// talks to a network and the one place `AGENTS.md` expects a proxy to produce
/// an `UnknownIssuer` somebody has to be shown.
///
/// Failing to install it is not a reason to stop, and it is the only failure
/// here worth a line of its own: a server with no log bridge still translates,
/// and one that refused to start over a bridge translates nothing. `config.rs`
/// and `model_pack.rs` treat a setup that did not come together the same way -
/// say what happened, carry on.
pub fn init() {
    let value = std::env::var(LOG_VARIABLE).ok();
    let (filter, discarded) = Filter::from_env_value(value.as_deref());

    // Read before the filter is moved into the logger, and set once. `log`'s
    // macros consult this ceiling before they build a record at all, so it has
    // to be the loudest level any directive allows - and it can be a single
    // answer for the same reason `register_callsite` can: the filter is settled
    // here and never changes.
    let ceiling = as_log(filter.max_level());
    let logger = Arc::new(Logger::stderr(filter));

    let _ = tracing::subscriber::set_global_default(Arc::clone(&logger));

    match log::set_boxed_logger(Box::new(logger)) {
        // Only once the logger is ours. The ceiling is global, and moving
        // someone else's would be filtering their log for them; upstream's
        // `LogTracer` builder sets it in this order for the same reason.
        Ok(()) => log::set_max_level(ceiling),
        Err(_) => tracing::warn!(
            "a `log` logger is already installed; records from the `log` crate will not be shown"
        ),
    }

    if let Some(value) = discarded {
        tracing::warn!("{LOG_VARIABLE} {value:?} is not a filter; logging at info");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// The filter `text` describes. Every use of this helper is a string the
    /// test expects to parse; the ones that must not are asserted on directly.
    fn filter(text: &str) -> Filter {
        Filter::parse(text).expect("the directives this test names are well formed")
    }

    /// Nothing set at all. A server that logged nothing by default would have
    /// nothing to point at when an editor says the language server died.
    #[test]
    fn an_unset_variable_means_info() {
        let (filter, discarded) = Filter::from_env_value(None);

        assert!(filter.allows("codegloss_lsp::config", &Level::INFO));
        assert!(!filter.allows("codegloss_lsp::config", &Level::DEBUG));
        assert_eq!(discarded, None);
    }

    /// `CODEGLOSS_LOG=` is someone clearing the variable, not someone asking
    /// for silence. Measured: `EnvFilter::try_new("")` builds an empty
    /// directive set and drops every event, errors included - which is the
    /// worst way for the thing we tell people to read when it breaks to fail.
    /// `model_pack::base_url` treats an empty value as unset for this reason.
    #[test]
    fn an_empty_variable_means_info_rather_than_silence() {
        for value in ["", "   "] {
            let (filter, discarded) = Filter::from_env_value(Some(value));

            assert!(filter.allows("codegloss_lsp", &Level::INFO), "{value:?}");
            assert!(!filter.allows("codegloss_lsp", &Level::DEBUG), "{value:?}");
            assert_eq!(discarded, None, "{value:?}");
        }
    }

    #[test]
    fn a_bare_level_applies_to_every_target() {
        let filter = filter("debug");

        assert!(filter.allows("codegloss_lsp::translation", &Level::DEBUG));
        assert!(filter.allows("tower_lsp_server", &Level::DEBUG));
        assert!(!filter.allows("codegloss_lsp::translation", &Level::TRACE));
    }

    /// The spellings are `LevelFilter::from_str`'s rather than a table here, so
    /// every `RUST_LOG` habit carries over: any case, and the numbers 0-5.
    #[test]
    fn a_level_may_be_spelled_in_any_case_or_as_a_number() {
        for text in ["debug", "DEBUG", "Debug", "4"] {
            let filter = filter(text);

            assert!(filter.allows("codegloss_lsp", &Level::DEBUG), "{text:?}");
            assert!(!filter.allows("codegloss_lsp", &Level::TRACE), "{text:?}");
        }
    }

    /// `off` is the one level that drops errors too.
    #[test]
    fn off_drops_even_errors() {
        let filter = filter("off");

        assert!(!filter.allows("codegloss_lsp::config", &Level::ERROR));
        assert_eq!(filter.max_level(), LevelFilter::OFF);
    }

    #[test]
    fn a_named_target_overrides_the_bare_level() {
        let filter = filter("warn,codegloss_lsp::translation=debug");

        assert!(filter.allows("codegloss_lsp::translation", &Level::DEBUG));
        assert!(!filter.allows("codegloss_lsp::config", &Level::DEBUG));
        assert!(filter.allows("codegloss_lsp::config", &Level::WARN));
    }

    /// The one case that rules out reading the directives as "any of them may
    /// let an event through". `DirectiveSet::enabled` takes
    /// `directives_for(meta).next()` - the first match in a list sorted by
    /// target length, so the most specific one, alone. Measured against
    /// `EnvFilter` itself: with `debug,codegloss_lsp::translation=warn` a
    /// `debug` event on `codegloss_lsp::translation` is dropped, even though
    /// the bare `debug` would allow it on its own.
    #[test]
    fn the_most_specific_target_decides_alone_even_when_it_is_the_quieter_one() {
        let filter = filter("debug,codegloss_lsp::translation=warn");

        assert!(!filter.allows("codegloss_lsp::translation", &Level::DEBUG));
        assert!(filter.allows("codegloss_lsp::translation", &Level::WARN));
        assert!(filter.allows("codegloss_lsp::config", &Level::DEBUG));
    }

    /// Specificity is the length of the target, not the position in the list:
    /// both orderings answer the same.
    #[test]
    fn the_longer_of_two_matching_targets_wins_whichever_was_written_first() {
        let general_first = filter("codegloss_lsp=debug,codegloss_lsp::translation=warn");
        let specific_first = filter("codegloss_lsp::translation=warn,codegloss_lsp=debug");

        for filter in [&general_first, &specific_first] {
            assert!(!filter.allows("codegloss_lsp::translation", &Level::DEBUG));
            assert!(filter.allows("codegloss_lsp::config", &Level::DEBUG));
        }
    }

    /// Naming a target and no bare level makes the list a whitelist, as it does
    /// in `EnvFilter`: measured, `codegloss_lsp=debug` drops another crate's
    /// `error` entirely.
    #[test]
    fn without_a_bare_level_an_unnamed_target_is_silent() {
        let filter = filter("codegloss_lsp=debug");

        assert!(filter.allows("codegloss_lsp::translation", &Level::DEBUG));
        assert!(!filter.allows("tower_lsp_server", &Level::ERROR));
    }

    /// A target matches by bytes and not by module segment - `EnvFilter`'s
    /// rule, kept so that a `RUST_LOG` habit means the same thing here.
    #[test]
    fn a_target_matches_on_any_prefix_rather_than_on_whole_segments() {
        let filter = filter("codegloss=debug");

        assert!(filter.allows("codegloss_lsp::translation", &Level::DEBUG));
    }

    /// `EnvFilter` reads a word it cannot parse as a level as a target name at
    /// `trace` instead, so a typo becomes `dbug=trace` - a filter that matches
    /// nothing and, having no bare level, silences everything. Measured: zero
    /// lines. Silence is the failure this module exists to avoid, so a level
    /// that is not one is an error and the caller falls back to info.
    #[test]
    fn a_misspelled_level_is_rejected_rather_than_read_as_a_target() {
        assert!(Filter::parse("dbug").is_err());
        assert!(Filter::parse("codegloss_lsp=dbug").is_err());
    }

    #[test]
    fn a_directive_with_nothing_before_the_equals_sign_is_rejected() {
        assert!(Filter::parse("=debug").is_err());
        assert!(Filter::parse("warn, =debug").is_err());
    }

    /// `codegloss_lsp=` gets `LevelFilter::from_str`'s answer for the empty
    /// string, which is `ERROR`. `EnvFilter` special-cases it to `TRACE` before
    /// it ever calls `from_str`; that special case is the first line of a
    /// second copy of the level table, and this module has no second copy. The
    /// divergence errs quiet rather than loud.
    #[test]
    fn a_directive_with_nothing_after_the_equals_sign_means_error() {
        let filter = filter("codegloss_lsp=");

        assert!(filter.allows("codegloss_lsp::config", &Level::ERROR));
        assert!(!filter.allows("codegloss_lsp::config", &Level::WARN));
    }

    /// Unlike `EnvFilter`, which rejects `codegloss = debug` outright
    /// (measured), spaces around either separator are dropped. Someone
    /// spacing out a shell variable is not making a different request.
    #[test]
    fn whitespace_and_empty_directives_are_ignored() {
        let spaced = filter("  info , codegloss_lsp::translation = debug ,, ");

        assert_eq!(spaced, filter("info,codegloss_lsp::translation=debug"));
        assert!(spaced.allows("codegloss_lsp::translation", &Level::DEBUG));
        assert!(!spaced.allows("codegloss_lsp::config", &Level::DEBUG));
    }

    /// Two directives for the same thing: the last one written wins, which is
    /// what `EnvFilter` does by replacing the equal entry in its sorted set.
    #[test]
    fn a_repeated_directive_is_settled_by_the_last_one_written() {
        let bare = filter("debug,info");
        assert!(!bare.allows("codegloss_lsp", &Level::DEBUG));

        let targeted = filter("codegloss_lsp=debug,codegloss_lsp=warn");
        assert!(!targeted.allows("codegloss_lsp", &Level::DEBUG));
        assert!(targeted.allows("codegloss_lsp", &Level::WARN));
    }

    /// What the subscriber offers as `max_level_hint`: the loudest level any
    /// directive could allow, so that `tracing` can skip callsites entirely.
    #[test]
    fn the_maximum_level_is_the_loudest_directive() {
        assert_eq!(filter("warn,x=debug").max_level(), LevelFilter::DEBUG);
        assert_eq!(filter("debug,x=warn").max_level(), LevelFilter::DEBUG);
        assert_eq!(filter("warn").max_level(), LevelFilter::WARN);
        assert_eq!(Filter::default().max_level(), LevelFilter::INFO);
    }

    /// A string that does not parse falls back to info and is handed back
    /// rather than complained about here: there is no subscriber to complain
    /// to yet, so the warning would be written nowhere.
    #[test]
    fn an_unreadable_variable_falls_back_to_info_and_is_handed_back() {
        let (filter, discarded) = Filter::from_env_value(Some(" dbug "));

        assert!(filter.allows("codegloss_lsp", &Level::INFO));
        assert!(!filter.allows("codegloss_lsp", &Level::DEBUG));
        assert_eq!(discarded.as_deref(), Some(" dbug "));
    }

    #[test]
    fn a_readable_variable_is_used_as_written() {
        let (filter, discarded) = Filter::from_env_value(Some("debug"));

        assert!(filter.allows("codegloss_lsp", &Level::DEBUG));
        assert_eq!(discarded, None);
    }

    /// The complaint names the half that failed. A whole string quoted back
    /// leaves the reader to find the typo in it themselves.
    #[test]
    fn the_error_names_the_one_directive_that_failed() {
        let message = Filter::parse("warn,codegloss_lsp=dbug")
            .expect_err("`dbug` is not a level")
            .to_string();

        assert!(message.contains("codegloss_lsp=dbug"), "{message}");
        assert!(!message.contains("warn"), "{message}");
    }

    /// The instant `secs` seconds after the epoch, plus `nanos`. A negative
    /// `secs` counts backwards from the epoch and the fraction is still added
    /// on top, which is the same pair of numbers the formatter itself works
    /// in: `-1` and `500_000_000` together are half a second before the epoch.
    fn at(secs: i64, nanos: u32) -> SystemTime {
        let whole = Duration::from_secs(secs.unsigned_abs());
        let fraction = Duration::from_nanos(u64::from(nanos));

        if secs < 0 {
            UNIX_EPOCH - whole + fraction
        } else {
            UNIX_EPOCH + whole + fraction
        }
    }

    /// What [`write_timestamp`] writes for that instant.
    fn formatted(secs: i64, nanos: u32) -> String {
        let mut out = String::new();
        write_timestamp(&mut out, at(secs, nanos));
        out
    }

    #[test]
    fn the_epoch_is_written_as_the_first_moment_of_1970() {
        assert_eq!(formatted(0, 0), "1970-01-01T00:00:00.000000Z");
    }

    /// 2000 is divisible by 400, so it is a leap year and February has a 29th.
    #[test]
    fn the_year_2000_has_a_leap_day() {
        assert_eq!(formatted(951_782_400, 0), "2000-02-29T00:00:00.000000Z");
    }

    /// 2100 is divisible by 100 but not by 400, so it is not a leap year: the
    /// day after February 28th is March 1st. A rule that only asks `year % 4`
    /// answers `2100-02-29` here and puts every date after it one day out -
    /// which nothing would notice for another 74 years.
    #[test]
    fn the_year_2100_has_no_leap_day() {
        assert_eq!(formatted(4_107_456_000, 0), "2100-02-28T00:00:00.000000Z");
        assert_eq!(formatted(4_107_542_400, 0), "2100-03-01T00:00:00.000000Z");
    }

    /// The same rule going backwards: 1900 was not a leap year either.
    #[test]
    fn the_year_1900_has_no_leap_day() {
        assert_eq!(formatted(-2_203_977_600, 0), "1900-02-28T00:00:00.000000Z");
        assert_eq!(formatted(-2_203_891_200, 0), "1900-03-01T00:00:00.000000Z");
    }

    /// The last second of a year belongs to that year.
    #[test]
    fn a_year_ends_one_second_before_the_next_one_starts() {
        assert_eq!(formatted(1_704_067_199, 0), "2023-12-31T23:59:59.000000Z");
        assert_eq!(formatted(1_704_067_200, 0), "2024-01-01T00:00:00.000000Z");
    }

    /// Before the epoch, `duration_since` hands back an `Err` holding the
    /// distance forwards to it, so the count has to be turned around and a
    /// fraction borrowed from the second below. Getting the borrow wrong shows
    /// up as a whole second, and a truncating division as a whole day.
    #[test]
    fn a_time_before_the_epoch_counts_backwards_from_it() {
        assert_eq!(formatted(-1, 0), "1969-12-31T23:59:59.000000Z");
        assert_eq!(formatted(-1, 500_000_000), "1969-12-31T23:59:59.500000Z");
        assert_eq!(formatted(-2_208_988_800, 0), "1900-01-01T00:00:00.000000Z");
        assert_eq!(formatted(-2_208_988_801, 0), "1899-12-31T23:59:59.000000Z");
    }

    /// Six digits, and the nanoseconds below them are dropped rather than
    /// rounded - `nanos / 1_000` in upstream's `Display`. Rounding would carry
    /// `999_999_999` up to `1_000_000` and write a seventh digit.
    #[test]
    fn a_fraction_of_a_second_is_truncated_to_six_digits() {
        assert_eq!(formatted(1_767_225_600, 0), "2026-01-01T00:00:00.000000Z");
        assert_eq!(formatted(1_767_225_600, 1), "2026-01-01T00:00:00.000000Z");
        assert_eq!(
            formatted(1_767_225_600, 123_456_789),
            "2026-01-01T00:00:00.123456Z"
        );
        assert_eq!(
            formatted(1_767_225_600, 999_999_999),
            "2026-01-01T00:00:00.999999Z"
        );
    }

    /// A year that does not fit in four digits is upstream's business too:
    /// `DateTime`'s `Display` writes a `+` and no padding above 9999, and pads
    /// to five below zero so that the sign has a column of its own. Only a
    /// broken clock gets here, but matching it costs three lines.
    ///
    /// The pre-1601 rows are skipped on Windows, as upstream skips them: since
    /// Rust 1.94 `SystemTime` subtraction panics there when the result would
    /// fall before the Windows epoch. That is the arithmetic in `at`, not the
    /// formatter.
    #[test]
    fn a_year_outside_four_digits_keeps_its_sign_and_its_padding() {
        assert_eq!(formatted(253_402_300_799, 0), "9999-12-31T23:59:59.000000Z");
        assert_eq!(
            formatted(253_402_300_800, 0),
            "+10000-01-01T00:00:00.000000Z"
        );

        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(formatted(-62_167_219_200, 0), "0000-01-01T00:00:00.000000Z");
            assert_eq!(
                formatted(-62_167_219_201, 0),
                "-0001-12-31T23:59:59.000000Z"
            );
        }
    }

    /// A sink a test can read back. The `Arc` is what lets the test keep hold
    /// of the bytes after the logger has taken the writer.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("no test panics while holding the sink")
                .extend_from_slice(data);

            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Capture {
        fn text(&self) -> String {
            let bytes = self
                .0
                .lock()
                .expect("no test panics while holding the sink")
                .clone();

            String::from_utf8(bytes).expect("the logger hands the sink a `String`")
        }
    }

    /// The instant every test's clock stands at, so that the expected line can
    /// be written out in full rather than matched around a moving part.
    fn frozen() -> SystemTime {
        UNIX_EPOCH
    }

    /// The start of every line these tests produce.
    const PREFIX: &str = "1970-01-01T00:00:00.000000Z  INFO codegloss_lsp::logging::tests: ";

    /// What the logger writes while `emit` runs, under `filter`.
    fn logged(filter: Filter, emit: impl FnOnce()) -> String {
        let capture = Capture::default();
        let logger = Logger::to(filter, capture.clone(), frozen);

        tracing::subscriber::with_default(logger, emit);

        capture.text()
    }

    /// The whole line, byte for byte. The two spaces after the timestamp are
    /// not a typo: the level sits in five columns and `INFO` is four wide.
    #[test]
    fn an_event_is_a_time_a_level_a_target_a_message_and_the_fields() {
        let line = logged(Filter::default(), || {
            tracing::info!(model_version = "x", "starting");
        });

        assert_eq!(
            line,
            "1970-01-01T00:00:00.000000Z  INFO codegloss_lsp::logging::tests: \
             starting model_version=\"x\"\n"
        );
    }

    /// The message leads whatever else was named, wherever it was written:
    /// every arm of `event!` that takes one expands to
    /// `{ message = format_args!(..), $($fields)* }`.
    #[test]
    fn the_message_comes_before_the_other_fields() {
        let line = logged(Filter::default(), || {
            tracing::info!(first = 1, second = 2, "said");
        });

        assert_eq!(line, format!("{PREFIX}said first=1 second=2\n"));
    }

    /// Five columns, right-aligned, so that the targets line up down the pane.
    #[test]
    fn a_level_sits_in_five_columns() {
        let lines = logged(filter("trace"), || {
            tracing::trace!("a");
            tracing::debug!("a");
            tracing::info!("a");
            tracing::warn!("a");
            tracing::error!("a");
        });

        let levels: Vec<&str> = lines
            .lines()
            .map(|line| &line["1970-01-01T00:00:00.000000Z ".len()..][..5])
            .collect();

        assert_eq!(levels, ["TRACE", "DEBUG", " INFO", " WARN", "ERROR"]);
    }

    /// `%value` is a `Display` value and goes in bare; a string field keeps the
    /// quotes `Debug` puts round it, because that is where the value visibly
    /// ends.
    #[test]
    fn a_display_value_loses_the_quotes_a_string_field_keeps() {
        let error = "the pack is missing";

        let line = logged(Filter::default(), || {
            tracing::info!(%error, path = "/tmp/a b", count = 3, ready = false, "gave up");
        });

        assert_eq!(
            line,
            format!(
                "{PREFIX}gave up error=the pack is missing path=\"/tmp/a b\" \
                 count=3 ready=false\n"
            )
        );
    }

    /// The message is prose and is copied out as prose: an `=` in it does not
    /// turn into a field, a newline in it is not escaped, and an empty one
    /// leaves the line ending at the target.
    #[test]
    fn a_message_is_copied_out_as_it_was_written() {
        let written = logged(Filter::default(), || {
            tracing::info!("");
            tracing::info!("a=b c=d");
            tracing::info!("two\nlines");
        });

        assert_eq!(
            written,
            format!("{PREFIX}\n{PREFIX}a=b c=d\n{PREFIX}two\nlines\n")
        );
    }

    /// The message is the one part of the line written without `Debug`'s
    /// escaping, so it is the only place a control character could reach a
    /// terminal intact. A log line is not a way to repaint the lines above it.
    #[test]
    fn a_control_character_in_a_message_is_spelled_out() {
        let written = logged(Filter::default(), || {
            tracing::info!("before\u{1b}[2Jafter\u{7f}");
        });

        assert_eq!(written, format!("{PREFIX}before\\x1b[2Jafter\\x7f\n"));
    }

    /// `r#type` is how Rust spells a field called `type`. The line writes the
    /// name, not the spelling.
    #[test]
    fn a_raw_identifier_loses_its_spelling() {
        let written = logged(Filter::default(), || {
            tracing::info!(r#type = "pack", "loaded");
        });

        assert_eq!(written, format!("{PREFIX}loaded type=\"pack\"\n"));
    }

    /// An error field brings the chain under it, under a second name made from
    /// the first. Nothing in this workspace records one - every site writes
    /// `%error`, which is a `Display` value - but a dependency may, and then
    /// the chain is the half worth having.
    #[test]
    fn an_error_field_brings_its_sources() {
        #[derive(Debug)]
        struct Layered(&'static str, Option<Box<Layered>>);

        impl fmt::Display for Layered {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.0)
            }
        }

        impl std::error::Error for Layered {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref().map(|error| error as _)
            }
        }

        let failure = Layered(
            "the pack could not be opened",
            Some(Box::new(Layered("no such file", None))),
        );

        let written = logged(Filter::default(), || {
            tracing::info!(
                cause = &failure as &(dyn std::error::Error + 'static),
                "gave up"
            );
        });

        assert_eq!(
            written,
            format!(
                "{PREFIX}gave up cause=the pack could not be opened \
                 cause.sources=[no such file]\n"
            )
        );
    }

    #[test]
    fn every_event_is_its_own_line() {
        let written = logged(Filter::default(), || {
            tracing::info!("first");
            tracing::info!("second");
        });

        assert_eq!(written, format!("{PREFIX}first\n{PREFIX}second\n"));
    }

    #[test]
    fn an_event_the_filter_drops_writes_nothing() {
        let written = logged(filter("warn"), || {
            tracing::info!("not this one");
        });

        assert_eq!(written, "");
    }

    /// The metadata of a real callsite, which is the only thing `enabled` and
    /// `register_callsite` will take. `Span::metadata` is the way to one that
    /// does not need a `Callsite` of our own - and the span has to be built
    /// under a subscriber that allows it, or the macro hands back a span with
    /// no metadata at all.
    fn a_debug_callsite() -> &'static Metadata<'static> {
        let logger = Logger::to(filter("trace"), Capture::default(), frozen);

        tracing::subscriber::with_default(logger, || {
            tracing::debug_span!("a callsite")
                .metadata()
                .expect("the span was built under a subscriber that allows it")
        })
    }

    /// `tracing` caches what `register_callsite` says for the life of the
    /// process, which is sound only because the filter is settled in `init`.
    /// `Interest::never` is what turns a `debug!` nobody asked for into a load
    /// and a branch.
    #[test]
    fn a_callsite_is_settled_once_and_for_all() {
        let capture = Capture::default();
        let allowed = Logger::to(filter("debug"), capture.clone(), frozen);
        let dropped = Logger::to(filter("warn"), capture, frozen);
        let callsite = a_debug_callsite();

        assert!(allowed.enabled(callsite));
        assert!(allowed.register_callsite(callsite).is_always());
        assert!(!dropped.enabled(callsite));
        assert!(dropped.register_callsite(callsite).is_never());
    }

    /// A hint that is too low silences events the filter would have let
    /// through, because `tracing` drops callsites above it without asking
    /// `enabled` at all.
    #[test]
    fn the_hint_is_the_loudest_level_any_directive_allows() {
        let logger = Logger::to(filter("warn,x=debug"), Capture::default(), frozen);

        assert_eq!(logger.max_level_hint(), Some(LevelFilter::DEBUG));
    }

    /// Nothing in this workspace opens a span, but a dependency may, and
    /// `span::Id::from_u64` panics on zero. The line carries no scope either
    /// way: without a registry there is nowhere to have kept one.
    #[test]
    fn a_span_gets_an_id_of_its_own_and_writes_no_line() {
        let capture = Capture::default();
        let logger = Logger::to(Filter::default(), capture.clone(), frozen);

        let (outer, inner) = tracing::subscriber::with_default(logger, || {
            let outer = tracing::info_span!("outer");
            let inner = tracing::info_span!("inner");

            outer.follows_from(&inner);
            outer.in_scope(|| inner.in_scope(|| tracing::info!("inside")));

            (outer.id(), inner.id())
        });

        assert!(outer.is_some() && inner.is_some());
        assert_ne!(outer, inner);
        assert_eq!(capture.text(), format!("{PREFIX}inside\n"));
    }

    /// Runs `use_it` on a `log` record built from the pieces. A record has to
    /// be used where it is built: `format_args!` borrows temporaries that live
    /// no longer than the statement they are written in.
    fn with_record(
        level: log::Level,
        target: &str,
        message: &str,
        use_it: impl FnOnce(&log::Record<'_>),
    ) {
        use_it(
            &log::Record::builder()
                .level(level)
                .target(target)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    /// A sink that counts flushes and keeps nothing. `Capture` answers what was
    /// written; `flush` is the one method whose whole point is the call itself.
    #[derive(Clone, Default)]
    struct Flushes(Arc<Mutex<usize>>);

    impl io::Write for Flushes {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            *self.0.lock().expect("no test panics while counting") += 1;

            Ok(())
        }
    }

    impl Flushes {
        /// How often the sink has been asked to flush.
        fn count(&self) -> usize {
            *self.0.lock().expect("no test panics while counting")
        }
    }

    /// `tracing_subscriber`'s `init` also installed `tracing_log::LogTracer`,
    /// so a record from a crate that logs through the `log` facade came out on
    /// the same lines as everything else. Under `--features candle` those
    /// crates are ureq, rustls and tokenizers - which is to say the model-pack
    /// download, the one place a person has to be told what went wrong.
    #[test]
    fn a_log_record_is_written_as_the_same_line_an_event_would_be() {
        let capture = Capture::default();
        let logger = Logger::to(Filter::default(), capture.clone(), frozen);

        with_record(log::Level::Info, "ureq::tls", "connecting", |record| {
            log::Log::log(&logger, record);
        });

        assert_eq!(
            capture.text(),
            "1970-01-01T00:00:00.000000Z  INFO ureq::tls: connecting\n"
        );
    }

    /// The five levels are the same five on both sides, and they land in the
    /// same five columns: `tracing_log`'s `AsTrace`, which is the only mapping
    /// the two crates could agree on.
    #[test]
    fn a_log_record_keeps_its_level() {
        let capture = Capture::default();
        let logger = Logger::to(filter("trace"), capture.clone(), frozen);

        for level in [
            log::Level::Trace,
            log::Level::Debug,
            log::Level::Info,
            log::Level::Warn,
            log::Level::Error,
        ] {
            with_record(level, "ureq", "a", |record| log::Log::log(&logger, record));
        }

        let written = capture.text();
        let levels: Vec<&str> = written
            .lines()
            .map(|line| &line["1970-01-01T00:00:00.000000Z ".len()..][..5])
            .collect();

        assert_eq!(levels, ["TRACE", "DEBUG", " INFO", " WARN", "ERROR"]);
    }

    /// The `log!` macros never call [`log::Log::enabled`] - they check only the
    /// ceiling, which knows nothing of targets - so a bridge that did not ask
    /// the filter itself would make `CODEGLOSS_LOG` mean nothing on this side.
    /// The target is the record's own, so `RUST_LOG` habits carry across:
    /// `ureq=debug` names ureq's records and nobody else's.
    #[test]
    fn a_log_record_is_filtered_on_its_own_target() {
        let capture = Capture::default();
        let logger = Logger::to(filter("warn,ureq=debug"), capture.clone(), frozen);

        for target in ["ureq::tls", "rustls::client::hs"] {
            with_record(log::Level::Debug, target, "a", |record| {
                log::Log::log(&logger, record);
            });
        }

        assert_eq!(
            capture.text(),
            "1970-01-01T00:00:00.000000Z DEBUG ureq::tls: a\n"
        );
    }

    /// `log`'s macros consult the ceiling before they build a record at all, so
    /// one that is too low silences records the filter would have let through -
    /// the same trap as `max_level_hint`, on the other side of the bridge.
    /// `off` is the value `log` has and `tracing`'s five levels do not.
    #[test]
    fn the_ceiling_is_the_loudest_level_any_directive_allows() {
        assert_eq!(
            as_log(filter("warn,ureq=debug").max_level()),
            log::LevelFilter::Debug
        );
        assert_eq!(as_log(filter("off").max_level()), log::LevelFilter::Off);
        assert_eq!(
            as_log(Filter::default().max_level()),
            log::LevelFilter::Info
        );
        assert_eq!(as_log(filter("trace").max_level()), log::LevelFilter::Trace);
        assert_eq!(as_log(filter("error").max_level()), log::LevelFilter::Error);
    }

    /// Upstream's `LogTracer::flush` did nothing, because the sink was not its
    /// to flush. This one owns it, and whoever calls `log::logger().flush()` is
    /// asking for the bytes to be gone before something else happens.
    #[test]
    fn flushing_the_bridge_flushes_the_sink() {
        let flushes = Flushes::default();
        let logger = Logger::to(Filter::default(), flushes.clone(), frozen);

        assert_eq!(flushes.count(), 0);
        log::Log::flush(&logger);
        assert_eq!(flushes.count(), 1);
    }

    /// stdout carries the JSON-RPC stream, and one stray byte there corrupts
    /// the protocol. Naming the type is what makes swapping in `io::stdout()`
    /// a compile error rather than a language server an editor quietly kills.
    #[test]
    fn the_logger_init_installs_writes_to_stderr() {
        let _: Logger<io::Stderr> = Logger::stderr(Filter::default());
    }
}
