//! The log sink: a `tracing` subscriber of this crate's own.
//!
//! `tracing` is only a facade - it records events and hands them to whatever
//! subscriber is installed, and with none installed every `tracing::info!` in
//! this workspace is a no-op. `tracing-subscriber` is the usual answer, and it
//! brought eight crates with it (`matchers`, `nu-ansi-term`, `sharded-slab`,
//! `thread_local`, `tracing-log`, `lazy_static`, `log`, and itself) for a
//! server that writes plain lines to stderr and has no spans at all.
//!
//! So this module is the subscriber. What it gives up against
//! `tracing-subscriber` is written down under [`Filter`]; the output format is
//! byte-for-byte the one `tracing_subscriber::fmt()` produced, because people's
//! eyes and greps are already trained on it:
//!
//! ```text
//! 2026-09-25T19:47:10.627704Z  INFO codegloss_lsp: starting model_version="passthrough-1"
//! ```
//!
//! IMPORTANT: stdout carries the JSON-RPC stream. A single stray byte there
//! corrupts the protocol and the editor kills the server, so everything here
//! goes to stderr and this crate must never `println!`.

use std::fmt;
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span;
use tracing::{Event, Metadata, Subscriber};

/// Environment variable holding the filter.
pub const FILTER_VARIABLE: &str = "CODEGLOSS_LOG";

/// What is printed when [`FILTER_VARIABLE`] says nothing.
const DEFAULT_LEVEL: LevelFilter = LevelFilter::INFO;

/// Installs this subscriber as the global default.
///
/// Called once, before anything logs. A second call is ignored rather than
/// fatal: losing the log is not worth taking the server down for.
pub fn init() {
    let directives = std::env::var(FILTER_VARIABLE).unwrap_or_default();
    let (filter, rejected) = Filter::parse(&directives);

    // Reported rather than swallowed, and reported without going through
    // `tracing` - the subscriber that would carry it is not installed yet.
    // A bad directive does not stop the server, for the same reason a
    // misspelled `--precision` does not: the log level is not worth a refusal.
    for directive in rejected {
        let _ = writeln!(
            std::io::stderr(),
            "{FILTER_VARIABLE}: ignoring {directive:?}, which is not a level or target=level"
        );
    }

    let logger = Logger {
        filter,
        sink: Box::new(Stderr),
        clock: SystemTime::now,
        next_span: AtomicU64::new(1),
    };
    // An error here means something already installed one. That only happens
    // if this is called twice, and the first one is as good as the second.
    let _ = tracing::subscriber::set_global_default(logger);
}

/// Which events are printed.
///
/// A deliberately small slice of what `tracing_subscriber::EnvFilter` accepts:
/// a bare level, and `target=level` for a target and everything under it,
/// comma-separated. **Spans, field values and per-callsite directives are not
/// understood** - nothing in this workspace opens a span, and the filtering
/// that is actually needed is coarse.
///
/// Targets are worth keeping, though. `tower-lsp-server` emits 83 events of its
/// own, so `CODEGLOSS_LOG=debug` is not the same request as
/// `CODEGLOSS_LOG=codegloss_lsp=debug`, and losing the second one would mean
/// reading the protocol layer's chatter every time this server is debugged.
#[derive(Debug, PartialEq, Eq)]
struct Filter {
    /// Applied to a target no rule names.
    default: LevelFilter,
    /// `(target, level)`, longest target first so the first match is the most
    /// specific one.
    targets: Vec<(String, LevelFilter)>,
}

impl Filter {
    /// Reads a directive list, and returns it beside the entries it could not
    /// read.
    ///
    /// A later directive wins over an earlier one for the same target, which is
    /// what someone appending to the variable means.
    fn parse(directives: &str) -> (Self, Vec<String>) {
        let mut filter = Self {
            default: DEFAULT_LEVEL,
            targets: Vec::new(),
        };
        let mut rejected = Vec::new();

        for directive in directives.split(',').map(str::trim) {
            if directive.is_empty() {
                continue;
            }
            match directive.split_once('=') {
                None => match level(directive) {
                    Some(level) => filter.default = level,
                    None => rejected.push(directive.to_owned()),
                },
                Some((target, text)) => match (target.trim(), level(text.trim())) {
                    ("", _) | (_, None) => rejected.push(directive.to_owned()),
                    (target, Some(level)) => {
                        filter.targets.retain(|(named, _)| named != target);
                        filter.targets.push((target.to_owned(), level));
                    }
                },
            }
        }

        // Longest first, so `level_for` can stop at its first match. Sorting
        // here rather than searching for the longest match on every event: the
        // list is fixed once this returns, and `enabled` runs per callsite.
        filter
            .targets
            .sort_by_key(|(named, _)| std::cmp::Reverse(named.len()));
        (filter, rejected)
    }

    /// The level a target is held to.
    ///
    /// A rule covers the target it names and everything under it, so
    /// `codegloss_lsp` also answers for `codegloss_lsp::config`. The boundary
    /// has to be a module separator: without that check `codegloss_ls` would
    /// claim `codegloss_lsp`.
    fn level_for(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(named, _)| covers(named, target))
            .map_or(self.default, |&(_, level)| level)
    }

    /// The most verbose level any rule allows.
    ///
    /// `tracing` asks for this once and uses it to skip callsites globally, so
    /// it has to be the maximum rather than the default - answering `INFO`
    /// while a rule says `debug` would silence that rule.
    fn max(&self) -> LevelFilter {
        self.targets
            .iter()
            .map(|&(_, level)| level)
            .chain(std::iter::once(self.default))
            .max()
            .unwrap_or(DEFAULT_LEVEL)
    }
}

/// Whether a rule for `named` applies to `target`.
fn covers(named: &str, target: &str) -> bool {
    target == named || (target.starts_with(named) && target[named.len()..].starts_with("::"))
}

/// Reads a level name, case-insensitively.
fn level(text: &str) -> Option<LevelFilter> {
    // `eq_ignore_ascii_case` rather than lower-casing: no allocation, and a
    // level name is ASCII by construction.
    [
        ("off", LevelFilter::OFF),
        ("error", LevelFilter::ERROR),
        ("warn", LevelFilter::WARN),
        ("info", LevelFilter::INFO),
        ("debug", LevelFilter::DEBUG),
        ("trace", LevelFilter::TRACE),
    ]
    .into_iter()
    .find(|(name, _)| text.eq_ignore_ascii_case(name))
    .map(|(_, level)| level)
}

/// Where a finished line goes.
///
/// A trait so that the tests can read what was written. The line arrives
/// complete and without its newline; an implementation must not split it,
/// because two threads logging at once would then interleave.
trait Sink: Send + Sync + 'static {
    fn write_line(&self, line: &str);
}

/// The real one.
struct Stderr;

impl Sink for Stderr {
    fn write_line(&self, line: &str) {
        // Locked so the newline cannot be separated from its line, and failures
        // dropped: a server that dies because its log pipe closed is worse than
        // one that goes quiet.
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
    }
}

struct Logger {
    filter: Filter,
    sink: Box<dyn Sink>,
    /// Injected so the tests can pin the timestamp.
    clock: fn() -> SystemTime,
    next_span: AtomicU64,
}

impl Subscriber for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // `<=` because `tracing` orders levels by severity, not by verbosity:
        // `ERROR < WARN < INFO < DEBUG < TRACE`. `the_levels_go_the_way_this_
        // file_assumes` pins it.
        *metadata.level() <= self.filter.level_for(metadata.target())
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.filter.max())
    }

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let mut line = Line::default();
        event.record(&mut line);

        let mut rendered = String::with_capacity(line.message.len() + line.fields.len() + 64);
        let _ = write!(
            rendered,
            "{} {:>5} {}: {}",
            timestamp((self.clock)()),
            metadata.level(),
            metadata.target(),
            line.message,
        );
        // The fields already carry their own leading space, which is one space
        // too many when there was no message to separate them from.
        rendered.push_str(if line.message.is_empty() {
            line.fields.trim_start()
        } else {
            &line.fields
        });

        self.sink.write_line(&rendered);
    }

    // Nothing in this workspace opens a span. These exist because the trait
    // requires them; the ids are handed out so that a span opened by a
    // dependency still gets a well-formed one back (`Id` may not be zero)
    // rather than a panic.
    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
    }
    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}
}

/// One event's fields, rendered.
#[derive(Default)]
struct Line {
    /// The `message` field, without a name and without quotes.
    message: String,
    /// Everything else, each as ` name=value`.
    fields: String,
}

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // Everything funnels through `Debug`, which is what reproduces the
        // format people are used to: a `&str` field comes out quoted
        // (`Visit::record_str` defaults to this method), a number bare, and the
        // message bare as well because it arrives as `fmt::Arguments`, whose
        // `Debug` is its `Display`.
        let _ = if field.name() == "message" {
            write!(self.message, "{value:?}")
        } else {
            write!(self.fields, " {}={:?}", field.name(), value)
        };
    }
}

/// `now` as RFC 3339 with microseconds, e.g. `2026-09-25T19:47:10.627704Z`.
///
/// Written out rather than taken from `time` or `chrono`, which is the whole
/// point of this module. The civil date comes from the days-to-y/m/d algorithm
/// that treats March as the first month, so that a leap day lands at the end of
/// a year and needs no special case.
fn timestamp(now: SystemTime) -> String {
    // A clock set before 1970 is not worth a branch in every log line: the
    // epoch is wrong but ordered, and the reader can see it is wrong.
    let since_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let microseconds = since_epoch.subsec_micros();

    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{microseconds:06}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60,
    )
}

/// Turns days since 1970-01-01 into a civil year, month and day.
///
/// Howard Hinnant's `civil_from_days`. The shift to a March-based year is what
/// makes it branchless: 29 February becomes the last day of the year, so the
/// leap day never moves anything that follows it.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // 719468 = days from 0000-03-01 to 1970-01-01.
    let shifted = days + 719_468;
    // An era is 400 years, which is the Gregorian cycle: exactly 146097 days.
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    // Months run March (0) to February (11) in this frame.
    let shifted_month = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = u32::try_from(day_of_year - (153 * shifted_month + 2) / 5 + 1).unwrap_or(1); // [1, 31]
    let month = u32::try_from(shifted_month + if shifted_month < 10 { 3 } else { -9 }).unwrap_or(1); // [1, 12]
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tracing::Level;

    use super::*;

    /// `enabled` compares with `<=`, which is only right because `tracing`
    /// orders levels by severity rather than by verbosity. Nothing in this file
    /// would fail to compile if that were the other way round - every event
    /// would simply come out inverted - so it is pinned here.
    #[test]
    fn the_levels_go_the_way_this_file_assumes() {
        assert!(Level::ERROR < Level::WARN);
        assert!(Level::WARN < Level::INFO);
        assert!(Level::INFO < Level::DEBUG);
        assert!(Level::DEBUG < Level::TRACE);
        assert!(LevelFilter::OFF < LevelFilter::ERROR);
        assert!(LevelFilter::ERROR < LevelFilter::TRACE);
    }

    #[test]
    fn nothing_configured_is_info() {
        let (filter, rejected) = Filter::parse("");
        assert_eq!(filter.level_for("codegloss_lsp"), LevelFilter::INFO);
        assert!(rejected.is_empty());
    }

    #[test]
    fn a_bare_level_moves_the_default() {
        let (filter, rejected) = Filter::parse("debug");
        assert_eq!(filter.level_for("anything"), LevelFilter::DEBUG);
        assert!(rejected.is_empty());
    }

    #[test]
    fn a_level_is_read_whatever_its_case() {
        for text in ["warn", "WARN", "Warn"] {
            let (filter, rejected) = Filter::parse(text);
            assert_eq!(filter.level_for("x"), LevelFilter::WARN, "in {text:?}");
            assert!(rejected.is_empty(), "in {text:?}");
        }
    }

    /// The reason targets are supported at all: `tower-lsp-server` logs 83
    /// events of its own, so turning this server up must not turn the protocol
    /// layer up with it.
    #[test]
    fn a_target_rule_leaves_everything_else_at_the_default() {
        let (filter, rejected) = Filter::parse("codegloss_lsp=debug,warn");
        assert_eq!(filter.level_for("codegloss_lsp"), LevelFilter::DEBUG);
        assert_eq!(
            filter.level_for("codegloss_lsp::config"),
            LevelFilter::DEBUG
        );
        assert_eq!(filter.level_for("tower_lsp_server"), LevelFilter::WARN);
        assert!(rejected.is_empty());
    }

    /// A rule covers what is under it, and the boundary is a module separator.
    /// Matching on the prefix alone would have `codegloss_ls` claim
    /// `codegloss_lsp`, which is a typo silently taking effect.
    #[test]
    fn a_rule_stops_at_a_module_boundary() {
        let (filter, _) = Filter::parse("codegloss_ls=trace");
        assert_eq!(filter.level_for("codegloss_ls"), LevelFilter::TRACE);
        assert_eq!(filter.level_for("codegloss_ls::inner"), LevelFilter::TRACE);
        assert_eq!(filter.level_for("codegloss_lsp"), LevelFilter::INFO);
    }

    #[test]
    fn the_most_specific_rule_wins_however_it_was_ordered() {
        for directives in [
            "codegloss_lsp=trace,codegloss_lsp::config=off",
            "codegloss_lsp::config=off,codegloss_lsp=trace",
        ] {
            let (filter, _) = Filter::parse(directives);
            assert_eq!(
                filter.level_for("codegloss_lsp::config"),
                LevelFilter::OFF,
                "in {directives:?}"
            );
            assert_eq!(
                filter.level_for("codegloss_lsp::backend"),
                LevelFilter::TRACE,
                "in {directives:?}"
            );
        }
    }

    #[test]
    fn a_later_rule_replaces_an_earlier_one_for_the_same_target() {
        let (filter, _) = Filter::parse("codegloss_lsp=trace,codegloss_lsp=warn");
        assert_eq!(filter.level_for("codegloss_lsp"), LevelFilter::WARN);
        assert_eq!(filter.targets.len(), 1);
    }

    /// A typo must not take the server down, and must not vanish either: the
    /// rest of the directives still apply and the bad one is handed back for
    /// `init` to report.
    #[test]
    fn an_unreadable_directive_is_reported_and_skipped() {
        let (filter, rejected) = Filter::parse("nonsense,debug,codegloss_lsp=loud,=warn");
        assert_eq!(filter.level_for("codegloss_lsp"), LevelFilter::DEBUG);
        assert_eq!(rejected, ["nonsense", "codegloss_lsp=loud", "=warn"]);
    }

    #[test]
    fn blank_entries_and_spaces_are_not_directives() {
        let (filter, rejected) = Filter::parse(" codegloss_lsp = debug , , warn ");
        assert_eq!(filter.level_for("codegloss_lsp"), LevelFilter::DEBUG);
        assert_eq!(filter.level_for("other"), LevelFilter::WARN);
        assert!(rejected.is_empty());
    }

    /// `tracing` uses this to skip callsites globally, so it has to be the most
    /// verbose level anything asks for. Answering with the default would
    /// silence the very rule that asked for more.
    #[test]
    fn the_hint_is_the_most_verbose_rule_not_the_default() {
        let (filter, _) = Filter::parse("warn,codegloss_lsp=trace");
        assert_eq!(filter.max(), LevelFilter::TRACE);

        let (filter, _) = Filter::parse("trace,codegloss_lsp=off");
        assert_eq!(filter.max(), LevelFilter::TRACE);
    }

    fn at(seconds: u64) -> String {
        timestamp(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    /// Expected values taken from `date -u -d @<seconds>`, not from this
    /// implementation.
    #[test]
    fn the_clock_reads_as_rfc_3339() {
        assert_eq!(at(0), "1970-01-01T00:00:00.000000Z");
        assert_eq!(at(1_234_567_890), "2009-02-13T23:31:30.000000Z");
        // A leap day, the 400-year rule (2000 is a leap year), the century that
        // is not (2100 is not), and the day after a leap February.
        assert_eq!(at(1_709_208_000), "2024-02-29T12:00:00.000000Z");
        assert_eq!(at(951_825_600), "2000-02-29T12:00:00.000000Z");
        assert_eq!(at(4_102_444_800), "2100-01-01T00:00:00.000000Z");
        assert_eq!(at(4_107_542_400), "2100-03-01T00:00:00.000000Z");
        assert_eq!(at(1_583_020_800), "2020-03-01T00:00:00.000000Z");
    }

    #[test]
    fn the_clock_keeps_microseconds_and_pads_them() {
        let now = UNIX_EPOCH + Duration::from_micros(1_234_567_890_000_007);
        assert_eq!(timestamp(now), "2009-02-13T23:31:30.000007Z");
    }

    /// A clock set before 1970 reads as the epoch rather than panicking or
    /// wrapping. Wrong, but ordered, and visibly wrong to whoever reads it.
    #[test]
    fn a_clock_before_the_epoch_does_not_panic() {
        let now = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(timestamp(now), "1970-01-01T00:00:00.000000Z");
    }

    #[derive(Default)]
    struct Recorder(Arc<Mutex<Vec<String>>>);

    impl Sink for Recorder {
        fn write_line(&self, line: &str) {
            self.0
                .lock()
                .expect("no test panics here")
                .push(line.to_owned());
        }
    }

    /// Emits events through the real `tracing` macros and reads back what the
    /// sink was handed, which is the only way to prove the rendering: the
    /// format is decided by `Visit`, and a hand-built `Event` cannot exercise
    /// it.
    fn lines_from(directives: &str, emit: impl FnOnce()) -> Vec<String> {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (filter, _) = Filter::parse(directives);
        let logger = Logger {
            filter,
            sink: Box::new(Recorder(Arc::clone(&written))),
            clock: || UNIX_EPOCH + Duration::from_micros(1_234_567_890_000_007),
            next_span: AtomicU64::new(1),
        };

        tracing::subscriber::with_default(logger, emit);
        written.lock().expect("no test panics here").clone()
    }

    /// The format `tracing_subscriber::fmt()` produced, pinned byte for byte.
    /// It was captured from the running server before that crate was dropped,
    /// and people's greps are trained on it.
    #[test]
    fn an_event_reads_exactly_as_it_did_under_tracing_subscriber() {
        let lines = lines_from("debug", || {
            tracing::info!(model_version = "passthrough-1", "starting");
        });
        assert_eq!(
            lines,
            [concat!(
                "2009-02-13T23:31:30.000007Z  INFO ",
                "codegloss_lsp::logging::tests: starting model_version=\"passthrough-1\""
            )]
        );
    }

    /// The three shapes the call sites actually use. A `&str` comes out quoted
    /// because `Visit::record_str` falls through to `Debug`; `%value` comes out
    /// bare because its `Debug` is its `Display`; a number comes out bare.
    #[test]
    fn a_field_is_quoted_or_bare_the_way_its_sigil_says() {
        let lines = lines_from("debug", || {
            tracing::info!(
                directory = %std::path::Path::new("/root/.cache").display(),
                name = "glosses",
                count = 0,
                "kept between runs"
            );
        });
        assert_eq!(
            lines[0].split(": ").nth(1).expect("the line has a body"),
            "kept between runs directory=/root/.cache name=\"glosses\" count=0"
        );
    }

    #[test]
    fn the_level_is_right_aligned_in_five_columns() {
        let lines = lines_from("trace", || {
            tracing::error!("a");
            tracing::warn!("b");
            tracing::info!("c");
            tracing::debug!("d");
            tracing::trace!("e");
        });
        let levels: Vec<&str> = lines
            .iter()
            .map(|line| &line["2009-02-13T23:31:30.000007Z ".len()..][..5])
            .collect();
        assert_eq!(levels, ["ERROR", " WARN", " INFO", "DEBUG", "TRACE"]);
    }

    #[test]
    fn an_event_with_only_fields_does_not_start_with_two_spaces() {
        let lines = lines_from("debug", || tracing::info!(count = 1));
        assert!(
            lines[0].ends_with(": count=1"),
            "the field ran on from the target with a doubled space: {:?}",
            lines[0]
        );
    }

    /// `max_level_hint` is a global cutoff: `tracing` consults it before it
    /// ever calls `enabled`, so answering with the default level would silence
    /// a target rule that asked for more. Nothing else here notices - the
    /// filter itself would still be right, and every directive would still
    /// parse - so the proof has to be an event that has to survive the cutoff.
    #[test]
    fn a_target_rule_survives_the_global_cutoff() {
        let lines = lines_from("warn,codegloss_lsp=debug", || {
            tracing::debug!("detail");
        });
        assert_eq!(
            lines.len(),
            1,
            "the debug rule was cut off before enabled() ran"
        );
    }

    #[test]
    fn the_filter_decides_what_reaches_the_sink() {
        let emit = || {
            tracing::debug!("quiet");
            tracing::warn!("loud");
        };
        assert_eq!(lines_from("debug", emit).len(), 2);
        assert_eq!(lines_from("warn", emit).len(), 1);
        assert_eq!(lines_from("off", emit).len(), 0);
    }
}
