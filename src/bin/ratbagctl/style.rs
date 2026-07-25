/* Terminal presentation for ratbagctl: decides whether ANSI SGR output is safe on a given stream
 * and renders the coloured, aligned primitives the command modules build their output from.
 *
 * Hand-rolled rather than pulled from a colour crate so the CLI keeps the daemon's minimal
 * dependency surface (see .agents/rules/rust-coding.md). The environment precedence below mirrors
 * anstream — the backend clap uses for its own help and error colouring — so our output and clap's
 * never disagree about whether colour is wanted. */

use std::io::IsTerminal;

use clap::ValueEnum;

/* SGR sequences. Kept private: callers reach them through the Style methods so that a
 * colour-disabled Style cannot be bypassed by accident. */
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[1;31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

/// When to emit ANSI colour.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    /// Colour when the stream is a terminal that looks capable.
    #[default]
    Auto,
    /// Always colour, even into a pipe.
    Always,
    /// Never colour.
    Never,
}

impl std::fmt::Display for ColorWhen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        };
        f.write_str(name)
    }
}

/// Semantic state markers shown after a list entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tag {
    Active,
    Default,
    Dirty,
    Disabled,
}

impl Tag {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "[active]",
            Self::Default => "[default]",
            Self::Dirty => "[dirty]",
            Self::Disabled => "[disabled]",
        }
    }
}

/// Presentation capabilities resolved for one output stream.
#[derive(Copy, Clone, Debug)]
pub struct Style {
    color: bool,
    truecolor: bool,
    unicode: bool,
}

impl Style {
    /// Style for stdout. `--json` output is never decorated: it has to stay parseable.
    pub fn for_stdout(when: ColorWhen, json: bool) -> Self {
        if json {
            Self::plain()
        } else {
            Self::detect(when, std::io::stdout().is_terminal())
        }
    }

    /// Style for stderr, resolved independently of stdout so that `ratbagctl … > file` still
    /// shows coloured errors on the terminal.
    pub fn for_stderr(when: ColorWhen) -> Self {
        Self::detect(when, std::io::stderr().is_terminal())
    }

    /// Undecorated, ASCII-only output.
    pub const fn plain() -> Self {
        Self { color: false, truecolor: false, unicode: false }
    }

    /* Precedence, matching anstream: NO_COLOR wins, then CLICOLOR_FORCE, then CLICOLOR=0, then
     * the terminal check. An explicit --color always/never short-circuits the lot. */
    fn detect(when: ColorWhen, is_tty: bool) -> Self {
        let color = match when {
            ColorWhen::Never => false,
            ColorWhen::Always => true,
            ColorWhen::Auto => {
                if env_nonempty("NO_COLOR") {
                    false
                } else if env_forced("CLICOLOR_FORCE") {
                    true
                } else if env_is_zero("CLICOLOR") {
                    false
                } else {
                    is_tty && term_supports_color()
                }
            }
        };
        Self { color, truecolor: color && env_truecolor(), unicode: utf8_locale() }
    }

    pub const fn is_color(self) -> bool {
        self.color
    }

    fn wrap(self, sgr: &str, text: &str) -> String {
        /* Wrapping nothing would emit escapes around an empty cell — invisible, but it shows up
         * in `cat -v` and in golden output. */
        if self.color && !text.is_empty() {
            format!("{sgr}{text}{RESET}")
        } else {
            text.to_string()
        }
    }

    /// Headings and object names.
    pub fn bold(self, text: &str) -> String {
        self.wrap(BOLD, text)
    }

    /// Secondary text: field labels, "supported" lists, old values.
    pub fn dim(self, text: &str) -> String {
        self.wrap(DIM, text)
    }

    /// Success, and the `[active]` state.
    pub fn good(self, text: &str) -> String {
        self.wrap(GREEN, text)
    }

    /// Needs-attention states: `[dirty]`, `[default]`, hint labels.
    pub fn warn(self, text: &str) -> String {
        self.wrap(YELLOW, text)
    }

    /// Failure.
    pub fn bad(self, text: &str) -> String {
        self.wrap(RED, text)
    }

    /// The value a write just set.
    pub fn accent(self, text: &str) -> String {
        self.wrap(CYAN, text)
    }

    pub fn tag(self, tag: Tag) -> String {
        match tag {
            Tag::Active => self.good(tag.label()),
            Tag::Default | Tag::Dirty => self.warn(tag.label()),
            Tag::Disabled => self.dim(tag.label()),
        }
    }

    /// A two-cell block filled with `rgb`, so a colour can be seen and not just read.
    ///
    /// Uses 24-bit colour where the terminal advertises it, else the nearest xterm 6×6×6 cube
    /// entry. Empty when colour is off — callers keep it in its own fixed-width column so the
    /// surrounding table stays aligned either way.
    pub fn swatch(self, r: u8, g: u8, b: u8) -> String {
        if !self.color {
            String::new()
        } else if self.truecolor {
            format!("\x1b[48;2;{r};{g};{b}m  {RESET}")
        } else {
            format!("\x1b[48;5;{}m  {RESET}", cube256(r, g, b))
        }
    }

    /// Display width of [`Style::swatch`], for padding the swatch column.
    pub const fn swatch_width(self) -> usize {
        if self.color { 2 } else { 0 }
    }

    pub fn ok_mark(self) -> String {
        if self.unicode { self.good("✓") } else { self.good("[ok]") }
    }

    pub fn info_mark(self) -> String {
        if self.unicode { self.dim("·") } else { self.dim("-") }
    }

    pub const fn arrow(self) -> &'static str {
        if self.unicode { "→" } else { "->" }
    }
}

/// Read `--color` out of the raw arguments, before clap parses them.
///
/// clap colours its own help and error output through anstream, which decides for itself. Handing
/// it our choice up front is the only way `--color never` also applies to a usage error. The
/// environment variables need no prescan: anstream already honours the same ones as
/// [`Style::detect`].
pub fn prescan_color<I, T>(args: I) -> ColorWhen
where
    I: IntoIterator<Item = T>,
    T: AsRef<std::ffi::OsStr>,
{
    let mut expecting_value = false;
    for arg in args {
        let arg = arg.as_ref().to_string_lossy().to_string();
        if expecting_value {
            return parse_when(&arg);
        }
        if let Some(value) = arg.strip_prefix("--color=") {
            return parse_when(value);
        }
        if arg == "--color" {
            expecting_value = true;
        } else if arg == "--" {
            /* Everything after this is a value, not an option. */
            break;
        }
    }
    ColorWhen::Auto
}

/* An unrecognised value falls back to auto; clap reports the real error a moment later. */
fn parse_when(value: &str) -> ColorWhen {
    match value.to_ascii_lowercase().as_str() {
        "never" | "no" | "off" => ColorWhen::Never,
        "always" | "yes" | "on" => ColorWhen::Always,
        _ => ColorWhen::Auto,
    }
}

/// The same choice, for clap's own help and error output.
pub const fn clap_color(when: ColorWhen) -> clap::ColorChoice {
    match when {
        ColorWhen::Auto => clap::ColorChoice::Auto,
        ColorWhen::Always => clap::ColorChoice::Always,
        ColorWhen::Never => clap::ColorChoice::Never,
    }
}

/* Nearest entry in the xterm 6x6x6 colour cube, for terminals without 24-bit colour. */
fn cube256(r: u8, g: u8, b: u8) -> u8 {
    let q = |v: u8| (u16::from(v) * 5 / 255) as u8;
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

fn env_nonempty(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty())
}

fn env_forced(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| !v.is_empty() && v != "0")
}

fn env_is_zero(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|v| v == "0")
}

fn term_supports_color() -> bool {
    match std::env::var_os("TERM") {
        Some(term) => !term.is_empty() && term != "dumb",
        None => false,
    }
}

fn env_truecolor() -> bool {
    match std::env::var_os("COLORTERM") {
        Some(v) => {
            let v = v.to_string_lossy().to_ascii_lowercase();
            v.contains("truecolor") || v.contains("24bit")
        }
        None => false,
    }
}

/* Decorative glyphs are only safe once we know the locale is UTF-8; LC_ALL=C and an unset
 * locale both fall back to ASCII. */
fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(std::env::var_os)
        .is_some_and(|v| {
            let v = v.to_string_lossy().to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

/// How a table cell is styled. Named semantically so the SGR constants stay private.
///
/// Cells whose colour varies within the cell — a run of state tags — use [`Cell::painted`]
/// instead.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Ink {
    #[default]
    Plain,
    Dim,
    Bold,
}

/// One table cell: the text to measure, plus how to paint it.
#[derive(Clone, Debug)]
pub struct Cell {
    text: String,
    ink: Ink,
    /// Pre-rendered content that carries its own escapes (a colour swatch), whose display
    /// width is `text`'s width rather than the string's length.
    raw: Option<String>,
}

impl Cell {
    pub fn new(text: impl AsRef<str>) -> Self {
        Self { text: text.as_ref().to_string(), ink: Ink::Plain, raw: None }
    }

    pub fn with(text: impl AsRef<str>, ink: Ink) -> Self {
        Self { text: text.as_ref().to_string(), ink, raw: None }
    }

    /// A cell whose rendered form is fixed (already contains escapes) but occupies `width`
    /// columns. Used for colour swatches.
    pub fn raw(rendered: String, width: usize) -> Self {
        Self { text: " ".repeat(width), ink: Ink::Plain, raw: Some(rendered) }
    }

    /// A cell that is already styled, measured by its unstyled text.
    ///
    /// For content whose colour varies per part — a run of state tags, where each tag has its own
    /// colour — so it cannot be described by a single [`Ink`].
    pub fn painted(plain: impl AsRef<str>, painted: String) -> Self {
        Self { text: plain.as_ref().to_string(), ink: Ink::Plain, raw: Some(painted) }
    }

    fn width(&self) -> usize {
        self.text.chars().count()
    }

    /* Pad first, paint second: padding a string that already contains SGR escapes is the
     * classic way to break column alignment. */
    fn render(&self, style: Style, width: usize) -> String {
        let pad = width.saturating_sub(self.width());
        let body = match &self.raw {
            Some(rendered) => {
                let mut s = rendered.clone();
                if rendered.is_empty() {
                    /* Colour is off, so the swatch collapsed: keep the column's spaces. */
                    s = self.text.clone();
                }
                s
            }
            None => match self.ink {
                Ink::Plain => self.text.clone(),
                Ink::Dim => style.dim(&self.text),
                Ink::Bold => style.bold(&self.text),
            },
        };
        format!("{body}{:pad$}", "", pad = pad)
    }
}

/// A column-aligned table. Widths are computed from unstyled text, so colour never shifts a
/// column.
#[derive(Default)]
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<Cell>>,
    gap: usize,
}

impl Table {
    pub fn new() -> Self {
        Self { headers: Vec::new(), rows: Vec::new(), gap: 2 }
    }

    pub fn headers<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.headers = headers.into_iter().map(Into::into).collect();
        self
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        self.rows.push(cells);
    }

    /// Render to a newline-separated block (no trailing newline).
    pub fn render(&self, style: Style) -> String {
        let columns = self
            .rows
            .iter()
            .map(Vec::len)
            .chain(std::iter::once(self.headers.len()))
            .max()
            .unwrap_or(0);

        let mut widths = vec![0usize; columns];
        for (i, header) in self.headers.iter().enumerate() {
            widths[i] = header.chars().count();
        }
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(cell.width());
            }
        }

        /* A column that ends up empty everywhere — the swatch column when colour is off — is
         * dropped along with its separator, so nothing indents by a phantom column. */
        let visible: Vec<usize> =
            (0..columns).filter(|index| widths[*index] > 0).collect();

        let sep = " ".repeat(self.gap);
        let mut out = Vec::with_capacity(self.rows.len() + 1);

        if !self.headers.is_empty() {
            let cells: Vec<String> = visible
                .iter()
                .map(|index| {
                    let header = self.headers.get(*index).cloned().unwrap_or_default();
                    Cell::with(header, Ink::Dim).render(style, widths[*index])
                })
                .collect();
            out.push(trim_end(&cells.join(&sep)));
        }

        for row in &self.rows {
            let cells: Vec<String> = visible
                .iter()
                .map(|index| match row.get(*index) {
                    Some(cell) => cell.render(style, widths[*index]),
                    None => " ".repeat(widths[*index]),
                })
                .collect();
            out.push(trim_end(&cells.join(&sep)));
        }

        out.join("\n")
    }
}

/* Trailing padding would leave invisible whitespace at end of line, which shows up in diffs
 * and in `cat -A`. Only spaces are trimmed, never escapes. */
fn trim_end(line: &str) -> String {
    line.trim_end_matches(' ').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled() -> Style {
        Style { color: true, truecolor: true, unicode: true }
    }

    #[test]
    fn explicit_choice_overrides_tty() {
        assert!(!Style::detect(ColorWhen::Never, true).is_color());
        assert!(Style::detect(ColorWhen::Always, false).is_color());
    }

    #[test]
    fn auto_needs_a_terminal() {
        /* Auto on a pipe must stay plain regardless of TERM. */
        assert!(!Style::detect(ColorWhen::Auto, false).is_color());
    }

    #[test]
    fn plain_style_emits_no_escapes() {
        let plain = Style::plain();
        assert_eq!(plain.bold("hi"), "hi");
        assert_eq!(plain.tag(Tag::Active), "[active]");
        assert!(plain.swatch(255, 0, 0).is_empty());
        assert_eq!(plain.arrow(), "->");
        assert_eq!(plain.ok_mark(), "[ok]");
    }

    #[test]
    fn styled_output_wraps_and_resets() {
        let style = styled();
        assert_eq!(style.bold("hi"), "\x1b[1mhi\x1b[0m");
        assert_eq!(style.swatch(1, 2, 3), "\x1b[48;2;1;2;3m  \x1b[0m");
        assert_eq!(style.arrow(), "→");
    }

    #[test]
    fn cube256_maps_corners() {
        assert_eq!(cube256(0, 0, 0), 16);
        assert_eq!(cube256(255, 255, 255), 231);
        assert_eq!(cube256(255, 0, 0), 196);
    }

    #[test]
    fn columns_align_without_color() {
        let mut table = Table::new().headers(["IDX", "NAME"]);
        table.row(vec![Cell::new("0"), Cell::new("short")]);
        table.row(vec![Cell::new("10"), Cell::new("much longer name")]);
        let out = table.render(Style::plain());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "IDX  NAME");
        assert_eq!(lines[1], "0    short");
        assert_eq!(lines[2], "10   much longer name");
    }

    #[test]
    fn columns_align_with_color() {
        /* The point of the exercise: a styled cell must occupy the same columns as a plain
         * one, so the visible text after the escapes still lines up. */
        let mut table = Table::new();
        table.row(vec![Cell::with("0", Ink::Bold), Cell::new("x")]);
        table.row(vec![Cell::new("1000"), Cell::new("y")]);
        let out = table.render(styled());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "\x1b[1m0\x1b[0m     x");
        assert_eq!(lines[1], "1000  y");

        let visible: Vec<usize> = lines
            .iter()
            .map(|l| strip_escapes(l).find('x').or_else(|| strip_escapes(l).find('y')).unwrap_or(0))
            .collect();
        assert_eq!(visible[0], visible[1]);
    }

    #[test]
    fn wide_characters_do_not_break_alignment_of_later_rows() {
        let mut table = Table::new();
        table.row(vec![Cell::new("Ünïcødé"), Cell::new("a")]);
        table.row(vec![Cell::new("ascii"), Cell::new("b")]);
        let out = table.render(Style::plain());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0].chars().position(|c| c == 'a'), lines[1].chars().position(|c| c == 'b'));
    }

    #[test]
    fn swatch_column_keeps_width_when_color_is_off() {
        let plain = Style::plain();
        let mut table = Table::new();
        table.row(vec![Cell::raw(plain.swatch(255, 0, 0), plain.swatch_width()), Cell::new("#ff0000")]);
        let out = table.render(plain);
        assert_eq!(out, "#ff0000");
    }

    fn strip_escapes(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
