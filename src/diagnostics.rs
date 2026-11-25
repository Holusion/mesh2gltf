//! Everything mesh2gltf writes to stderr. A host may show it to whoever supplied the file, so it
//! has to be safe to show as it stands, with nothing left for the host to strip.
//!
//! Each message has a level, and is written as text for people or, with `--log-format json`, as
//! one JSON object per line. The levels, formats and fields are meshtinker's (its `docs/logs.md`),
//! so that a caller running both reads their stderr the same way.
#[cfg(not(test))]
use std::io::{self, Write};
use std::{path::Path, sync::OnceLock};

/// Long enough for any message mesh2gltf composes. Text from a file is capped well below this.
const MAX_LINE: usize = 512;
const MAX_TEXT: usize = 64;
const MAX_EXAMPLES: usize = 3;

/// Levels by increasing severity. The names are eCorpus's task log levels, as in meshtinker, so
/// that a caller can pass one `--log-level` to both. mesh2gltf only writes `info`, `warn` and
/// `error`; `trace` and `debug` are accepted, and then show the same messages as `info`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
const LEVELS: [(&str, Level); 5] = [
    ("trace", Level::Trace),
    ("debug", Level::Debug),
    ("info", Level::Info),
    ("warn", Level::Warn),
    ("error", Level::Error),
];
impl Level {
    pub fn parse(name: &str) -> Option<Self> {
        LEVELS.iter().find(|(n, _)| *n == name).map(|&(_, l)| l)
    }
    fn name(self) -> &'static str {
        LEVELS.iter().find(|(_, l)| *l == self).unwrap().0
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Text,
    Json,
}
impl Format {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}
static SETTINGS: OnceLock<(Level, Format)> = OnceLock::new();
/// Set once, before the command line is checked, so that a wrong one is reported as asked.
pub fn configure(threshold: Level, format: Format) {
    let _ = SETTINGS.set((threshold, format));
}
fn settings() -> (Level, Format) {
    SETTINGS
        .get()
        .copied()
        .unwrap_or((Level::Info, Format::Text))
}

/// Writes an `info` message: what the conversion did, and its progress with `--progress`.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::diagnostics::log($crate::diagnostics::Level::Info, &format!($($arg)*), None)
    };
}
/// Writes a `warn` message: data omitted or changed, the conversion going on.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::diagnostics::log($crate::diagnostics::Level::Warn, &format!($($arg)*), None)
    };
}
/// Writes one message, if its level reaches `--log-level`. `code` is a stable code for callers.
pub fn log(level: Level, message: &str, code: Option<&str>) {
    let code = code.map(|code| ("code", code.into()));
    emit(level, message, code.as_slice());
}
/// Writes one step of `--progress`: `done` out of `total` of `stage`, one of a few stable names.
/// Text shows the percentage after the message. JSON has it as a fraction, so that a caller can
/// draw a bar per stage rather than print every line.
pub fn progress(stage: &'static str, done: u64, total: u64, message: &str) {
    let (message, fields) = progress_line(stage, done, total, message);
    emit(Level::Info, &message, &fields);
}
type Field = (&'static str, serde_json::Value);
fn progress_line(stage: &str, done: u64, total: u64, message: &str) -> (String, [Field; 2]) {
    let fraction = if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64).min(1.0)
    };
    let message = format!("{message} ({}%)", (fraction * 100.0).floor());
    // Three digits are finer than any bar, and keep the line short.
    let fraction = (fraction * 1000.0).floor() / 1000.0;
    (
        message,
        [("progress", fraction.into()), ("stage", stage.into())],
    )
}
fn emit(level: Level, message: &str, fields: &[Field]) {
    let (threshold, format) = settings();
    if level >= threshold {
        write(&formatted(level, message, fields, format));
    }
}
/// Text keeps the severity as a prefix, and leaves the other fields to JSON. JSON has `level`,
/// `message`, then the other fields, in the order meshtinker writes them.
fn formatted(level: Level, message: &str, fields: &[Field], format: Format) -> String {
    let message = sanitized(message);
    match format {
        Format::Text => match level {
            Level::Trace => format!("Trace: {message}"),
            Level::Debug => format!("Debug: {message}"),
            Level::Info => message,
            Level::Warn => format!("Warning: {message}"),
            Level::Error => format!("Error: {message}"),
        },
        Format::Json => {
            let mut line = format!(
                "{{\"level\":\"{}\",\"message\":{}",
                level.name(),
                serde_json::Value::from(message)
            );
            for (name, value) in fields {
                line.push_str(&format!(",\"{name}\":{value}"));
            }
            line.push('}');
            line
        }
    }
}
/// `eprintln!` would panic once a host has closed the pipe, turning a warning into a failed import.
fn write(line: &str) {
    let line = format!("{line}\n");
    #[cfg(test)]
    #[allow(clippy::print_stderr)]
    {
        // Only this macro's output is captured for a passing test.
        eprint!("{line}");
    }
    #[cfg(not(test))]
    let _ = io::stderr().write_all(line.as_bytes());
}
/// Rust's own panic report goes straight to stderr, and its message can quote the file. This one
/// goes through [`log`], with the backtrace when `RUST_BACKTRACE` asks for one.
pub fn report_panics() {
    std::panic::set_hook(Box::new(|info| {
        let message = format!("internal error: {}", info.to_string().replace('\n', " "));
        log(Level::Error, &message, Some("internal"));
        let backtrace = std::backtrace::Backtrace::capture();
        if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            for frame in backtrace.to_string().lines() {
                match settings().1 {
                    Format::Text => write(&sanitized(frame)),
                    Format::Json => log(Level::Error, frame, None),
                }
            }
        }
    }));
}
/// Text from a file, for use inside a message: names, keywords and references. Cleaned like any
/// message, and cut so that a message stays short whatever the file says.
pub fn safe(text: &str) -> String {
    let mut out: String = text.chars().take(MAX_TEXT).map(cleaned).collect();
    if text.chars().nth(MAX_TEXT).is_some() {
        let keep = out
            .char_indices()
            .nth(MAX_TEXT - 3)
            .map_or(out.len(), |(i, _)| i);
        out.truncate(keep);
        out.push_str("...");
    }
    out
}
/// One warning per kind of problem, however often a file repeats it: the count, and a few examples.
/// It holds no more than those, so it doesn't grow with the file. It reports when dropped, so that
/// what was found still shows if the import then fails.
pub struct Tally {
    what: &'static str,
    count: u64,
    examples: Vec<String>,
}
impl Tally {
    /// `what` comes before the count: "undefined materials".
    pub fn new(what: &'static str) -> Self {
        Self {
            what,
            count: 0,
            examples: Vec::new(),
        }
    }
    /// `example` may come from the file.
    pub fn add(&mut self, example: &str) {
        self.count += 1;
        if self.examples.len() < MAX_EXAMPLES {
            let example = safe(example);
            if !example.is_empty() && !self.examples.contains(&example) {
                self.examples.push(example);
            }
        }
    }
    fn message(&self) -> Option<String> {
        let (count, what) = (self.count, self.what);
        let examples = match self.examples.join(", ") {
            examples if examples.is_empty() => examples,
            examples => format!(", for example {examples}"),
        };
        (count > 0).then(|| format!("{what}: {count}{examples}"))
    }
}
impl Drop for Tally {
    fn drop(&mut self) {
        if let Some(message) = self.message() {
            log(Level::Warn, &message, None);
        }
    }
}
/// A path for a message: only its file name, so that the layout of the machine never shows, and
/// a long path can't push the name past the cap.
pub fn shown(path: &Path) -> String {
    let name = path.file_name().map(|name| name.to_string_lossy());
    safe(name.as_deref().unwrap_or("file"))
}
/// The backstop under every message, for text that no call site cleaned, such as what a library
/// quotes from the input in its error. One message stays on one line, and can neither drive a
/// terminal nor read as markup.
fn sanitized(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(MAX_LINE).map(cleaned).collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}
/// Control characters, markup, and the invisible format characters that reorder or hide text.
fn cleaned(c: char) -> char {
    let hostile = c.is_control()
        || matches!(c, '<' | '>' | '&')
        || matches!(c as u32, 0xAD | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF);
    if hostile {
        '?'
    } else {
        c
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_names_stay_as_written() {
        for name in [
            "wood_01.png",
            "textures/Mur (2).JPG",
            "m,1:a-b",
            "été 木",
            "l'église [v2]+#1.obj",
        ] {
            assert_eq!(safe(name), name);
        }
    }
    #[test]
    fn what_could_do_harm_in_a_name_is_replaced() {
        assert_eq!(safe("\x1b[31m0"), "?[31m0");
        assert_eq!(safe("<b>a&b\n"), "?b?a?b?");
        assert_eq!(safe("a\u{202e}b"), "a?b");
    }
    #[test]
    fn a_long_name_is_cut_to_the_cap() {
        let name = "é".repeat(MAX_TEXT);
        assert_eq!(safe(&name), name);
        let cut = safe(&"é".repeat(MAX_TEXT + 1));
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert!(cut.ends_with("é..."), "{cut}");
    }
    #[test]
    fn a_path_shows_only_its_name() {
        assert_eq!(shown(Path::new("/srv/job/textures/a.png")), "a.png");
        assert_eq!(shown(Path::new("textures/../a.png")), "a.png");
        assert_eq!(shown(Path::new("/")), "file");
    }
    #[test]
    fn a_tally_counts_everything_and_keeps_a_few_examples() {
        let mut tally = Tally::new("odd things");
        assert_eq!(tally.message(), None);
        for example in ["a", "a", "<b", "c", "d", "e"] {
            tally.add(example);
        }
        assert_eq!(
            tally.message().unwrap(),
            "odd things: 6, for example a, ?b, c"
        );
    }
    #[test]
    fn a_tally_with_only_empty_names_gives_no_examples() {
        let mut tally = Tally::new("odd things");
        tally.add("");
        assert_eq!(tally.message().unwrap(), "odd things: 1");
        tally.add("a");
        assert_eq!(tally.message().unwrap(), "odd things: 2, for example a");
    }
    #[test]
    fn a_message_stays_as_written() {
        let message = "Warning: skipped 3 faces; use --center (bounds 0 → 1.5) `#VRML` it's";
        assert_eq!(sanitized(message), message);
    }
    #[test]
    fn nothing_can_drive_a_terminal_or_read_as_markup() {
        assert_eq!(sanitized("\x1b[31mred\x07"), "?[31mred?");
        assert_eq!(sanitized("a\nb\r\tc"), "a?b??c");
        assert_eq!(sanitized("<img src=x>&amp;"), "?img src=x??amp;");
        assert_eq!(sanitized("a\u{202e}b\u{200b}c\u{feff}"), "a?b?c?");
    }
    #[test]
    fn a_long_message_is_cut() {
        let cut = sanitized(&"é".repeat(MAX_LINE + 1));
        assert_eq!(cut.chars().count(), MAX_LINE + 3);
        assert!(cut.ends_with("é..."), "{cut}");
        assert_eq!(sanitized(&"x".repeat(MAX_LINE)), "x".repeat(MAX_LINE));
    }
    #[test]
    fn levels_and_formats_have_meshtinkers_names() {
        for name in ["trace", "debug", "info", "warn", "error"] {
            assert_eq!(Level::parse(name).unwrap().name(), name);
        }
        assert!(Level::Debug < Level::Info && Level::Warn < Level::Error);
        assert_eq!(Level::parse("warning"), None);
        assert_eq!(Format::parse("json"), Some(Format::Json));
        assert_eq!(Format::parse("JSON"), None);
    }
    #[test]
    fn text_says_the_severity_first() {
        let code = [("code", "usage".into())];
        let text = |level| formatted(level, "a <b>", &code, Format::Text);
        assert_eq!(text(Level::Info), "a ?b?");
        assert_eq!(text(Level::Warn), "Warning: a ?b?");
        assert_eq!(text(Level::Error), "Error: a ?b?");
    }
    #[test]
    fn json_is_one_object_with_level_message_and_code() {
        let code = [("code", "rejected".into())];
        let line = formatted(Level::Error, "bad \"x\"\n", &code, Format::Json);
        assert_eq!(
            line,
            r#"{"level":"error","message":"bad \"x\"?","code":"rejected"}"#
        );
        let line = formatted(Level::Warn, "é", &[], Format::Json);
        assert_eq!(line, r#"{"level":"warn","message":"é"}"#);
        let fields = [("progress", 0.25.into()), ("stage", "read".into())];
        let line = formatted(Level::Info, "Read", &fields, Format::Json);
        assert_eq!(
            line,
            r#"{"level":"info","message":"Read","progress":0.25,"stage":"read"}"#
        );
    }
    #[test]
    fn progress_is_a_fraction_of_its_stage() {
        let line = |done, total| {
            let (message, fields) = progress_line("write", done, total, "Converted");
            formatted(Level::Info, &message, &fields, Format::Json)
        };
        assert_eq!(
            line(1, 3),
            r#"{"level":"info","message":"Converted (33%)","progress":0.333,"stage":"write"}"#
        );
        assert!(line(5, 3).contains(r#""progress":1.0,"#));
        assert!(line(0, 0).contains("(100%)"));
        let (message, fields) = progress_line("read", 999, 1000, "Read 1 kB");
        assert_eq!(
            formatted(Level::Info, &message, &fields, Format::Text),
            "Read 1 kB (99%)"
        );
    }
}
