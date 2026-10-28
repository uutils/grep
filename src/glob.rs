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
use uucore::error::{UResult, USimpleError};

const SEPARATOR: &str = if cfg!(windows) {
    r"[/\\]"
} else if cfg!(target_os = "uefi") {
    r"\\"
} else {
    "/"
};

/// Include and exclude patterns, in command-line order. The last matching
/// pattern decides; otherwise, skip only if the first pattern is an include.
#[derive(Default)]
pub struct GlobSet {
    patterns: Vec<(bool, OnigRegex)>,
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

    pub(crate) fn add(&mut self, include: bool, pattern: &str) -> UResult<()> {
        let regex = OnigRegex::compile(
            &translate(pattern),
            Syntax::perl_ng(),
            RegexOptions::REGEX_OPTION_MULTILINE,
        )
        .map_err(|err| {
            USimpleError::new(2, format!("invalid glob '{pattern}': {}", err.message))
        })?;
        self.patterns.push((include, regex));
        Ok(())
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

        let name = name.to_str().map(str::as_bytes);
        match self
            .patterns
            .iter()
            .rev()
            .find(|(_, pattern)| name.is_some_and(|name| matches(pattern, name)))
        {
            Some((include, _)) => !include,
            None => self.patterns[0].0,
        }
    }
}

fn translate(pattern: &str) -> String {
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
            '[' => translate_class(&mut chars, &mut out),
            _ if is_separator(c) => out.push_str(SEPARATOR),
            _ => push_literal(c, &mut out),
        }
    }
    out.push_str(r"\z");
    out
}

fn push_literal(c: char, out: &mut String) {
    if r"\.^$|?*+()[]{}-".contains(c) {
        out.push('\\');
    }
    out.push(c);
}

fn translate_class(chars: &mut Chars<'_>, out: &mut String) {
    let start = out.len();
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
                out.push(']');
                *chars = body;
                return;
            }
            '\\' => push_literal(body.next().unwrap_or('\\'), out),
            '[' => {
                let rest = body.as_str();
                if let Some(&delimiter @ (b':' | b'.' | b'=')) = rest.as_bytes().first()
                    && let Some(end) = find_bracket_subexpr_end(rest.as_bytes(), 1, delimiter)
                {
                    out.push('[');
                    out.push_str(&rest[..end]);
                    body = rest[end..].chars();
                } else {
                    out.push_str(r"\[");
                }
            }
            '/' if cfg!(windows) => out.push_str(r"/\\"),
            _ => out.push(c),
        }
        first = false;
    }

    // An unclosed bracket is literal; leave the remaining input unconsumed.
    out.truncate(start);
    out.push_str(r"\[");
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
        globs.add(false, pattern).unwrap();
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
    fn regex_errors_are_reported() {
        for pattern in ["[z-a]", "[[:notaclass:]]"] {
            let mut globs = GlobSet::new();
            let err = globs.add(false, pattern).unwrap_err();
            assert!(err.to_string().contains("invalid glob"), "{pattern:?}");
            assert!(globs.patterns.is_empty());
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
    }

    #[test]
    fn last_matching_pattern_wins_across_suffixes() {
        for include_last in [false, true] {
            let mut globs = GlobSet::new();
            globs.add(!include_last, "t/*").unwrap();
            globs.add(include_last, "a.txt").unwrap();
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
    fn non_utf8_names_keep_the_default_action() {
        use std::os::unix::ffi::OsStrExt;

        let name = OsStr::from_bytes(b"t/\xff/a.txt");
        assert!(!GlobSet::new().skips_command_line_file(Path::new(name)));
        for include in [false, true] {
            let mut globs = GlobSet::new();
            globs.add(include, "*.txt").unwrap();
            assert_eq!(globs.skips(name), include);
            assert_eq!(globs.skips_command_line_file(Path::new(name)), include);
        }
    }
}
