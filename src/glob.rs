// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! fnmatch-like glob handling. Translates glob patterns to Onig
//! regular expressions, to make `[:class:]` work correctly.

use crate::matcher::{OnigRegex, find_bracket_subexpr_end};
use onig::{RegexOptions, Syntax};
use std::ffi::OsStr;
use std::path::{Path, is_separator};
use std::str::Chars;

const SEPARATOR: &str = if cfg!(windows) { r"[/\\]" } else { "/" };

/// Include and exclude patterns, in command-line order. The last matching
/// pattern decides; otherwise, skip only if the first pattern is an include.
#[derive(Default)]
pub struct GlobSet {
    patterns: Vec<(bool, Option<OnigRegex>)>,
}

impl GlobSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            patterns: Vec::with_capacity(capacity),
        }
    }

    /// Like GNU, an invalid pattern such as `[z-a]` matches nothing.
    pub(crate) fn add(&mut self, include: bool, pattern: &str) {
        let regex = translate(pattern).and_then(|regex| {
            OnigRegex::compile(
                &regex,
                Syntax::perl_ng(),
                RegexOptions::REGEX_OPTION_MULTILINE,
            )
            .ok()
        });
        self.patterns.push((include, regex));
    }

    /// Match a recursive entry's basename, not its path suffixes.
    pub fn skips(&self, name: &OsStr) -> bool {
        self.skips_if(name, |regex, name| regex.match_at(name, 0, None).is_some())
    }

    /// Match the whole path or a nonempty suffix after a run of separators.
    pub fn skips_command_line_file(&self, path: &Path) -> bool {
        self.skips_if(path.as_os_str(), |regex, name| {
            regex.search(name, 0, None).is_some()
        })
    }

    fn skips_if(&self, name: &OsStr, matches: impl Fn(&OnigRegex, &[u8]) -> bool) -> bool {
        if self.patterns.is_empty() {
            return false;
        }

        let name = name.as_encoded_bytes();
        match self
            .patterns
            .iter()
            .rev()
            .find(|(_, pattern)| pattern.as_ref().is_some_and(|p| matches(p, name)))
        {
            Some((include, _)) => !include,
            None => self.patterns[0].0,
        }
    }
}

fn translate(pattern: &str) -> Option<String> {
    // Exclude empty suffixes and positions inside a run of separators, but
    // allow any pattern (including a leading separator) at the start of input.
    let mut out = format!(r"(?:\A|(?<={SEPARATOR})(?!{SEPARATOR}|\z))");
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '?' => out.push('.'),
            '*' => {
                out.push_str(".*");
                while chars.as_str().starts_with('*') {
                    chars.next();
                }
            }
            '\\' => push_literal(chars.next().unwrap_or('\\'), &mut out),
            '[' => translate_class(&mut chars, &mut out)?,
            _ if is_separator(c) => out.push_str(SEPARATOR),
            _ => push_literal(c, &mut out),
        }
    }
    out.push_str(r"\z");
    Some(out)
}

fn push_literal(c: char, out: &mut String) {
    if r"\.^$|?*+()[]{}-".contains(c) {
        out.push('\\');
    }
    out.push(c);
}

fn translate_class(chars: &mut Chars<'_>, out: &mut String) -> Option<()> {
    let start = out.len();
    let mut separator = false;
    let mut body = chars.clone();
    out.push('[');
    if body.as_str().starts_with(['!', '^']) {
        body.next();
        out.push('^');
    }
    let mut first = true;
    while let Some(c) = body.next() {
        match c {
            ']' if !first => {
                if separator {
                    out.push_str(r"\\");
                }
                out.push(']');
                *chars = body;
                return Some(());
            }
            '\\' => push_literal(body.next().unwrap_or('\\'), out),
            '[' => {
                let rest = body.as_str();
                if let Some(&delimiter @ (b':' | b'.' | b'=')) = rest.as_bytes().first()
                    && let Some(end) = find_bracket_subexpr_end(rest.as_bytes(), 1, delimiter)
                {
                    if delimiter == b':' {
                        out.push('[');
                        out.push_str(&rest[..end]);
                    } else {
                        // `[.a.]` and `[=a=]` stand for the character itself.
                        let mut inner = rest[1..end - 2].chars();
                        let (Some(c), None) = (inner.next(), inner.next()) else {
                            return None;
                        };
                        push_literal(c, out);
                    }
                    body = rest[end..].chars();
                } else {
                    out.push_str(r"\[");
                }
            }
            '/' if cfg!(windows) => {
                out.push('/');
                separator = true;
            }
            _ => out.push(c),
        }
        first = false;
    }

    // An unclosed bracket is literal; leave the remaining input unconsumed.
    out.truncate(start);
    out.push_str(r"\[");
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separator_matches_platform() {
        let regex = OnigRegex::compile(
            &format!(r"\A{SEPARATOR}\z"),
            Syntax::perl_ng(),
            RegexOptions::REGEX_OPTION_NONE,
        )
        .unwrap_or_else(|err| panic!("{}", err.message));
        for c in ['/', '\\', ':', 'a'] {
            assert_eq!(
                regex.match_at(c.to_string().as_bytes(), 0, None).is_some(),
                is_separator(c),
                "{c:?}",
            );
        }
    }

    fn excluding(pattern: &str) -> GlobSet {
        let mut globs = GlobSet::new();
        globs.add(false, pattern);
        globs
    }

    #[test]
    fn gnu_wildcards() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("*", &["", ".hidden", "a\nb"], &[]),
            ("**.txt", &["a.txt", "t/a.txt"], &["a.log"]),
            ("***.txt", &["a.txt", "t/a.txt"], &["a.log"]),
            ("t/**/a.txt", &["t/x/a.txt", "t/x/y/a.txt"], &["t/a.txt"]),
            ("t/*", &["t/a.txt", "t/x/a.txt"], &["x/a.txt"]),
            ("q?x", &["q/x", "qax", "q\nx"], &["qx", "qaax"]),
            (r"a\*.txt", &["a*.txt"], &["abc.txt"]),
            (r"a\?.txt", &["a?.txt"], &["ab.txt"]),
            (r"a\[b]", &["a[b]"], &["ab"]),
            (r"a\\b", &[r"a\b"], &["ab"]),
            (r"\a", &["a"], &[r"\a", "\x07"]),
            ("a\\", &["a\\"], &["a"]),
            (".^$+(){}|", &[".^$+(){}|"], &[""]),
        ];
        assert_cases(cases);
        assert_eq!(translate("***.txt"), translate("*.txt"));
    }

    #[test]
    fn gnu_brackets() {
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("[ab]", &["a", "b"], &["[ab]", "c"]),
            ("[!a]", &["b", "!"], &["a"]),
            ("[^a]", &["b", "^"], &["a"]),
            ("[", &["["], &["a"]),
            ("[]", &["[]"], &["]"]),
            ("[!]", &["[!]"], &["!"]),
            ("[abc", &["[abc"], &["a"]),
            ("[]]", &["]"], &["[", "[]]"]),
            ("[!]]", &["[", "a"], &["]"]),
            ("[[]", &["["], &["]"]),
            ("[a-z]", &["a", "m", "z"], &["A", "0"]),
            ("[-a]", &["-", "a"], &["z"]),
            ("[a-]", &["-", "a"], &["z"]),
            ("[a&&b]", &["a", "&", "b"], &["c"]),
            ("[[:digit:]]", &["0", "9"], &["a", "]"]),
            ("[![:digit:]]", &["a", "\n"], &["9"]),
            ("[[:alpha:]]", &["a", "\u{e9}"], &["9"]),
            (r"[\]]", &["]"], &[r"\", r"\]"]),
            (r"[\d]", &["d"], &["9"]),
            (r"[a\-z]", &["a", "-", "z"], &["m"]),
            ("[[.a.]]", &["a"], &[".]", "[.a.]"]),
            ("[[=a=]]", &["a"], &["=]", "[=a=]"]),
            ("[[.-.]]", &["-"], &["a"]),
        ];
        assert_cases(cases);
    }

    fn assert_cases(cases: &[(&str, &[&str], &[&str])]) {
        for &(pattern, matching, nonmatching) in cases {
            let globs = excluding(pattern);
            for (names, expected) in [(matching, true), (nonmatching, false)] {
                for name in names {
                    assert_eq!(
                        globs.skips(OsStr::new(name)),
                        expected,
                        "{pattern:?}, {name:?}",
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_patterns_match_nothing() {
        for pattern in ["[z-a]", "[[:notaclass:]]", "[[.ab.]]"] {
            for name in ["a", "z", "ab", pattern] {
                assert!(!excluding(pattern).skips(OsStr::new(name)), "{pattern:?}");
            }
        }
    }

    #[test]
    fn suffix_boundaries() {
        let globs = excluding("a.txt");
        for name in ["a.txt", "t/a.txt", "t//a.txt", "./t/a.txt"] {
            assert!(globs.skips_command_line_file(Path::new(name)), "{name}");
        }
        for name in ["data.txt", "t/data.txt", "t/a.txt\n", "a.txt/"] {
            assert!(!globs.skips_command_line_file(Path::new(name)), "{name}");
        }
        assert!(!globs.skips(OsStr::new("t/a.txt")));
        assert!(!excluding("/a.txt").skips_command_line_file(Path::new("t//a.txt")));
        assert!(!excluding("").skips_command_line_file(Path::new("t/")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_separators() {
        let globs = excluding("t/*.txt");
        assert!(globs.skips_command_line_file(Path::new(r"t\a.txt")));
        assert!(excluding("a.txt").skips_command_line_file(Path::new(r"t\\a.txt")));
        assert!(excluding("[/]").skips(OsStr::new(r"\")));
        assert!(excluding("[/-0].txt").skips(OsStr::new("0.txt")));
    }

    #[test]
    fn last_matching_pattern_wins_across_suffixes() {
        for include_last in [false, true] {
            let mut globs = GlobSet::new();
            globs.add(!include_last, "t/*");
            globs.add(include_last, "a.txt");
            assert_eq!(
                globs.skips_command_line_file(Path::new("t/a.txt")),
                !include_last,
            );
            assert_eq!(globs.skips(OsStr::new("unmatched")), !include_last);
        }
    }

    #[test]
    fn empty_set_keeps_files() {
        let globs = GlobSet::new();
        assert!(!globs.skips(OsStr::new("a")));
        assert!(!globs.skips_command_line_file(Path::new("t/a")));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_match() {
        use std::os::unix::ffi::OsStrExt;

        let name = OsStr::from_bytes(b"t\xff/a.txt");
        for pattern in ["*.txt", "a.txt", "t?/a.txt"] {
            assert!(excluding(pattern).skips_command_line_file(Path::new(name)));
        }
        assert!(excluding("*.txt").skips(OsStr::from_bytes(b"\xff.txt")));
    }
}
