// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::{Config, RegexMode};
use fancy_regex::{BytesMode, Regex, RegexBuilder};
use memchr::memmem;
use uucore::error::{UResult, USimpleError};
use uucore::show_warning;

pub struct Matcher<'a> {
    config: &'a Config<'a>,
    patterns: Vec<CompiledPattern>,
    /// One substring searcher per pattern, present only when *every* pattern is
    /// a plain literal that a raw byte search resolves exactly (see
    /// [`plain_literal`]). When set, a caller can decide a line matches by
    /// looking for any of these needles, bypassing the regex engine entirely.
    /// `None` as soon as a single pattern needs real regex evaluation.
    literal_searchers: Option<Vec<memmem::Finder<'static>>>,
}

impl<'a> Matcher<'a> {
    pub fn compile(config: &'a Config<'a>) -> UResult<Self> {
        let mut patterns = Vec::with_capacity(config.patterns.len());
        for raw in config.patterns {
            patterns.push(CompiledPattern::compile(raw, config)?);
        }

        // If we can reduce the whole pattern set to literal needles, keep a
        // searcher for each so the driver can take a bulk substring-scan path.
        let needles: Option<Vec<Vec<u8>>> = config
            .patterns
            .iter()
            .map(|p| plain_literal(p, config.ignore_case, config.regex_mode))
            .collect();
        let literal_searchers = needles.filter(|n| !n.is_empty()).map(|n| {
            n.iter()
                .map(|w| memmem::Finder::new(w).into_owned())
                .collect()
        });

        Ok(Self {
            config,
            patterns,
            literal_searchers,
        })
    }

    /// Per-pattern substring searchers, present only when the pattern set is a
    /// pure set of literals (no regex needed). Used by the searcher to scan a
    /// whole buffer at once instead of testing line by line.
    pub fn literal_searchers(&self) -> Option<&[memmem::Finder<'static>]> {
        self.literal_searchers.as_deref()
    }

    /// Decide whether `line` matches and return the positions to highlight.
    pub fn match_line(&self, line: &[u8]) -> Option<Vec<(usize, usize)>> {
        let mut any_seen = false;
        let mut any_selected = false;
        let positions: Vec<_> = MatchIter::new(&self.patterns, line)
            .filter(|&(start, end)| {
                any_seen = true;
                // Drop matches that don't span the whole line if `-x` was requested.
                if self.config.line_regexp && !(start == 0 && end == line.len()) {
                    return false;
                }
                // Drop matches that aren't word matches if `-w` was requested.
                if self.config.word_regexp && !Self::is_word_match(line, start, end) {
                    return false;
                }
                any_selected = true;
                // Drop zero-length matches from the output.
                if start == end {
                    return false;
                }
                true
            })
            .collect();

        let raw_matched = if self.config.line_regexp || self.config.word_regexp {
            // -w / -x are authoritative once matches are filtered. Zero-length
            // matches can select a line even though there is no span to output.
            any_selected
        } else {
            any_seen
        };

        if raw_matched != self.config.invert_match {
            Some(positions)
        } else {
            None
        }
    }

    /// Cheap match check that doesn't enumerate positions.
    pub fn is_match(&self, line: &[u8]) -> Option<Vec<(usize, usize)>> {
        // `-w` / `-x` need positions to filter, so we fall back to `match_line`.
        let matched = if self.config.line_regexp || self.config.word_regexp {
            self.match_line(line).is_some()
        } else {
            let raw_matched = self.patterns.iter().any(|p| p.is_match(line));
            raw_matched != self.config.invert_match
        };
        matched.then(Vec::new)
    }

    /// Word-boundary check `-w`.
    /// Start/End-of-line count as non-words.
    fn is_word_match(line: &[u8], start: usize, end: usize) -> bool {
        if let Some(&line_end) = line.get(end) {
            let next_char = match std::str::from_utf8(&line[end..]) {
                Ok(s) => s.chars().next(),
                Err(e) if e.valid_up_to() > 0 => {
                    std::str::from_utf8(&line[end..end + e.valid_up_to()])
                        .ok()
                        .and_then(|s| s.chars().next())
                }
                Err(_) => None,
            };
            if let Some(c) = next_char {
                if c.is_alphanumeric() || c == '_' {
                    return false;
                }
            } else if line_end.is_ascii_alphanumeric() || line_end == b'_' {
                return false;
            }
        }

        if start > 0 {
            let mut i = start - 1;
            while i > 0 && (line[i] & 0xC0) == 0x80 && start - i <= 4 {
                i -= 1;
            }
            let prev_char = match std::str::from_utf8(&line[i..start]) {
                Ok(s) => s.chars().last(),
                Err(_) => None,
            };
            if let Some(c) = prev_char {
                if c.is_alphanumeric() || c == '_' {
                    return false;
                }
            } else if line[start - 1].is_ascii_alphanumeric() || line[start - 1] == b'_' {
                return false;
            }
        }

        true
    }
}

/// Streaming k-way merge over compiled patterns
struct MatchIter<'a> {
    cursors: Vec<Cursor<'a>>,
    /// End of the last emitted match.
    last_end: usize,
}

impl<'a> MatchIter<'a> {
    fn new(patterns: &'a [CompiledPattern], line: &'a [u8]) -> Self {
        Self {
            cursors: patterns
                .iter()
                .map(|pattern| {
                    let mut c = Cursor {
                        pattern,
                        line,
                        offset: 0,
                        pending: None,
                    };
                    c.refill();
                    c
                })
                .collect(),
            last_end: 0,
        }
    }
}

impl<'a> Iterator for MatchIter<'a> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        // Discard stale pendings that fall before the last emit.
        for cursor in &mut self.cursors {
            if matches!(cursor.pending, Some((s, _)) if s < self.last_end) {
                cursor.offset = self.last_end;
                cursor.refill();
            }
        }

        // Pick the leftmost pending.
        // Tie-break by largest end so POSIX leftmost-longest holds across
        // patterns too (e.g. `-e a -e ab` against `ab` emits `ab`).
        let best_idx = self
            .cursors
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.pending.map(|p| (i, p)))
            .min_by_key(|&(_, (s, e))| (s, std::cmp::Reverse(e)))
            .map(|(i, _)| i)?;

        let (start, end) = self.cursors[best_idx].pending.unwrap();
        self.cursors[best_idx].refill();
        self.last_end = end;
        Some((start, end))
    }
}

struct Cursor<'a> {
    pattern: &'a CompiledPattern,
    line: &'a [u8],
    /// Where the next `search_leftmost` call should start.
    offset: usize,
    /// Pre-fetched next match for this pattern.
    /// `None` once the pattern is exhausted.
    pending: Option<(usize, usize)>,
}

impl Cursor<'_> {
    fn refill(&mut self) {
        if self.offset > self.line.len() {
            self.pending = None;
            return;
        }
        let Some((start, end)) = self.pattern.search_leftmost(self.line, self.offset) else {
            self.pending = None;
            return;
        };
        // Advance the next search past the match we just found.
        // Zero-length matches need a +1 nudge to avoid spinning forever.
        self.offset = end.max(start + 1);
        self.pending = Some((start, end));
    }
}

/// Return the literal bytes of `pattern` when a raw byte-for-byte substring
/// search is *exactly* equivalent to matching it, otherwise `None`.
fn plain_literal(pattern: &str, ignore_case: bool, mode: RegexMode) -> Option<Vec<u8>> {
    if ignore_case || pattern.is_empty() || !pattern.is_ascii() {
        return None;
    }
    const SPECIAL: &[u8] = b".*[]^$\\+?{}()|";
    let plain = mode == RegexMode::Fixed || !pattern.bytes().any(|b| SPECIAL.contains(&b));
    plain.then(|| pattern.as_bytes().to_vec())
}

struct CompiledPattern {
    regex: Regex,
}

impl CompiledPattern {
    fn compile(pattern: &str, config: &Config) -> UResult<Self> {
        if matches!(config.regex_mode, RegexMode::Basic | RegexMode::Extended)
            && has_confusing_bracket(pattern.as_bytes())
        {
            return Err(USimpleError::new(
                2,
                "character class syntax is [[:space:]], not [:space:]".to_string(),
            ));
        }

        let mut normalized_pattern = None;
        let pattern = if config.regex_mode == RegexMode::Extended {
            if let Some((op, rest)) = strip_leading_repeat_operator(pattern) {
                show_warning!("{op} at start of expression");
                normalized_pattern = Some(rest.to_string());
            }
            normalized_pattern.as_deref().unwrap_or(pattern)
        } else {
            pattern
        };

        let transpiled = match config.regex_mode {
            RegexMode::Fixed => fancy_regex::escape(pattern).into_owned(),
            RegexMode::Basic => transpile_bre(pattern)?,
            RegexMode::Extended => transpile_ere(pattern)?,
            RegexMode::Perl => pattern.to_string(),
        };

        let mut builder = RegexBuilder::new(&transpiled);
        builder.oniguruma_mode(true);
        builder.bytes_mode(BytesMode::UnicodeBytes);

        if config.ignore_case {
            builder.case_insensitive(true);
        }
        // In GNU grep's Basic/Extended modes, `-z` makes newline ordinary data
        // for `.`, but PCRE keeps its existing non-DOTALL behavior.
        if config.null_data && matches!(config.regex_mode, RegexMode::Basic | RegexMode::Extended) {
            builder.dot_matches_new_line(true);
        }

        let regex = builder.build().map_err(|err| {
            let dbg = format!("{err:?}");
            let message = if dbg.contains("ClassRangeInvalid")
                || dbg.contains("character class range")
                || dbg.contains("range out of order")
                || dbg.contains("Invalid range end")
            {
                "Invalid range end".to_string()
            } else {
                format!("invalid pattern \"{pattern}\": {err}")
            };
            USimpleError::new(2, message)
        })?;

        Ok(Self { regex })
    }

    /// Find the leftmost match starting at or after `offset`.
    fn search_leftmost(&self, line: &[u8], offset: usize) -> Option<(usize, usize)> {
        match self.regex.find_from_pos(line, offset) {
            Ok(Some(m)) => Some((m.start(), m.end())),
            _ => None,
        }
    }

    /// True if any match exists in `line` (including zero-length).
    fn is_match(&self, line: &[u8]) -> bool {
        self.regex.is_match(line).unwrap_or(false)
    }
}

fn map_posix_class(name: &str) -> Option<&'static str> {
    match name {
        "alpha" => Some(r"\p{Alphabetic}"),
        "lower" => Some(r"\p{Lowercase}"),
        "upper" => Some(r"\p{Uppercase}"),
        "alnum" => Some(r"\p{Alphabetic}0-9"),
        "space" => Some(r"\p{White_Space}"),
        "blank" => Some(r"\t\p{Zs}"),
        "cntrl" => Some(r"\p{Control}"),
        "digit" => Some("0-9"),
        "xdigit" => Some("0-9A-Fa-f"),
        "ascii" => Some(r"\x00-\x7F"),
        "word" => Some(r"\w"),
        "punct" => Some(r"\p{Punctuation}"),
        "graph" => Some(r"\P{C}&&\P{Z}"),
        "print" => Some(r"\P{C}"),
        _ => None,
    }
}

/// Convert POSIX Basic Regular Expression (BRE) to standard regex syntax for fancy-regex.
fn transpile_bre(pattern: &str) -> UResult<String> {
    let mut output = String::with_capacity(pattern.len() * 2);
    let mut chars = pattern.chars().peekable();
    let mut in_bracket = false;

    while let Some(ch) = chars.next() {
        if in_bracket {
            if ch == '[' && chars.peek() == Some(&':') {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == ':' && chars.peek() == Some(&']') {
                        chars.next();
                        closed = true;
                        break;
                    }
                    name.push(c);
                }
                if closed {
                    if let Some(unicode_class) = map_posix_class(&name) {
                        output.push_str(unicode_class);
                        continue;
                    } else {
                        return Err(USimpleError::new(
                            2,
                            "invalid character class name".to_string(),
                        ));
                    }
                } else {
                    output.push_str("[:");
                    output.push_str(&name);
                    continue;
                }
            }
            if ch == ']' && output.ends_with(|c| c != '\\' && c != '[' && c != '^') {
                in_bracket = false;
            }
            output.push(ch);
            continue;
        } else if ch == '[' {
            in_bracket = true;
            output.push(ch);
            continue;
        } else if ch != '\\' {
            match ch {
                '+' | '?' | '|' | '(' | ')' | '{' | '}' => {
                    output.push('\\');
                    output.push(ch);
                }
                '*' => {
                    if output.is_empty()
                        || output.ends_with('(')
                        || output.ends_with('|')
                        || output.ends_with('^')
                    {
                        output.push_str(r"\*");
                    } else {
                        output.push('*');
                    }
                }
                '^' => {
                    if output.is_empty() || output.ends_with('(') || output.ends_with('|') {
                        output.push('^');
                    } else {
                        output.push_str(r"\^");
                    }
                }
                '$' => {
                    let is_anchor = chars.peek().is_none()
                        || (chars.peek() == Some(&'\\')
                            && (chars.clone().nth(1) == Some(')')
                                || chars.clone().nth(1) == Some('|')));
                    if is_anchor {
                        output.push('$');
                    } else {
                        output.push_str(r"\$");
                    }
                }
                _ => output.push(ch),
            }
            continue;
        }

        match chars.next() {
            Some('{') => {
                output.push('{');
                if chars.peek() == Some(&',') {
                    output.push('0');
                }
            }
            Some(c) if "()}|+?".contains(c) => output.push(c),
            Some('`') => output.push_str(r"\A"),
            Some('\'') => output.push_str(r"\z"),
            Some('<') => output.push_str(r"\b(?=\w)"),
            Some('>') => output.push_str(r"(?<=\w)\b"),
            Some(c) => {
                output.push('\\');
                output.push(c);
            }
            None => {
                output.push('\\');
            }
        }
    }

    Ok(sort_alternation_branches(&output))
}

/// Convert POSIX Extended Regular Expression (ERE) with GNU extensions to standard syntax.
fn transpile_ere(pattern: &str) -> UResult<String> {
    let mut output = String::with_capacity(pattern.len() * 2);
    let mut chars = pattern.chars().peekable();
    let mut in_bracket = false;

    while let Some(ch) = chars.next() {
        if in_bracket {
            if ch == '[' && chars.peek() == Some(&':') {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                while let Some(c) = chars.next() {
                    if c == ':' && chars.peek() == Some(&']') {
                        chars.next();
                        closed = true;
                        break;
                    }
                    name.push(c);
                }
                if closed {
                    if let Some(unicode_class) = map_posix_class(&name) {
                        output.push_str(unicode_class);
                        continue;
                    } else {
                        return Err(USimpleError::new(
                            2,
                            "invalid character class name".to_string(),
                        ));
                    }
                } else {
                    output.push_str("[:");
                    output.push_str(&name);
                    continue;
                }
            }
            if ch == ']' && output.ends_with(|c| c != '\\' && c != '[' && c != '^') {
                in_bracket = false;
            }
            output.push(ch);
            continue;
        } else if ch == '[' {
            in_bracket = true;
            output.push(ch);
            continue;
        } else if ch != '\\' {
            if ch == '{' && chars.peek() == Some(&',') {
                output.push_str("{0,");
                chars.next();
                continue;
            }
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('`') => output.push_str(r"\A"),
            Some('\'') => output.push_str(r"\z"),
            Some('<') => output.push_str(r"\b(?=\w)"),
            Some('>') => output.push_str(r"(?<=\w)\b"),
            Some(c) => {
                output.push('\\');
                output.push(c);
            }
            None => output.push('\\'),
        }
    }

    Ok(sort_alternation_branches(&output))
}

/// Sort alternation branches by descending length so that leftmost-first engines
/// match the longest alternative first (POSIX leftmost-longest semantics).
fn sort_alternation_branches(pattern: &str) -> String {
    if !pattern.contains('|') {
        return pattern.to_string();
    }

    let chars: Vec<char> = pattern.chars().collect();
    sort_subexpr(&chars, 0, chars.len())
}

fn sort_subexpr(chars: &[char], start: usize, end: usize) -> String {
    let mut branches: Vec<String> = Vec::new();
    let mut current_branch = String::new();
    let mut i = start;
    let mut in_bracket = false;
    let mut bracket_start = 0;

    while i < end {
        let ch = chars[i];
        if ch == '\\' {
            if i + 1 < end {
                current_branch.push('\\');
                current_branch.push(chars[i + 1]);
                i += 2;
                continue;
            } else {
                current_branch.push('\\');
                i += 1;
                continue;
            }
        }

        if in_bracket {
            current_branch.push(ch);
            if ch == ']' && i != bracket_start {
                in_bracket = false;
            }
            i += 1;
            continue;
        }

        if ch == '[' {
            in_bracket = true;
            current_branch.push(ch);
            i += 1;
            bracket_start = i;
            if i < end && chars[i] == '^' {
                current_branch.push('^');
                i += 1;
                bracket_start = i;
            }
            continue;
        }

        if ch == '(' {
            let group_start = i + 1;
            let mut depth = 1;
            let mut j = i + 1;
            let mut b_in_bracket = false;
            let mut b_bracket_start = 0;
            while j < end && depth > 0 {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if b_in_bracket {
                    if chars[j] == ']' && j != b_bracket_start {
                        b_in_bracket = false;
                    }
                    j += 1;
                    continue;
                }
                if chars[j] == '[' {
                    b_in_bracket = true;
                    j += 1;
                    b_bracket_start = j;
                    if j < end && chars[j] == '^' {
                        j += 1;
                        b_bracket_start = j;
                    }
                    continue;
                }
                if chars[j] == '(' {
                    depth += 1;
                } else if chars[j] == ')' {
                    depth -= 1;
                }
                j += 1;
            }

            if depth == 0 {
                let group_end = j - 1;
                let inner = sort_subexpr(chars, group_start, group_end);
                current_branch.push('(');
                current_branch.push_str(&inner);
                current_branch.push(')');
                i = j;
                continue;
            } else {
                current_branch.push('(');
                i += 1;
                continue;
            }
        }

        if ch == '|' {
            branches.push(current_branch);
            current_branch = String::new();
            i += 1;
            continue;
        }

        current_branch.push(ch);
        i += 1;
    }

    branches.push(current_branch);

    if branches.len() > 1 {
        branches.sort_by_key(|a| std::cmp::Reverse(a.len()));
        branches.join("|")
    } else {
        branches.pop().unwrap_or_default()
    }
}

fn strip_leading_repeat_operator(pattern: &str) -> Option<(&'static str, &str)> {
    match pattern.as_bytes().first()? {
        b'?' => Some(("?", &pattern[1..])),
        b'*' => Some(("*", &pattern[1..])),
        b'+' => Some(("+", &pattern[1..])),
        b'{' => strip_leading_interval_repeat(pattern).map(|rest| ("{...}", rest)),
        _ => None,
    }
}

fn strip_leading_interval_repeat(pattern: &str) -> Option<&str> {
    let close = pattern.as_bytes().iter().position(|&b| b == b'}')?;
    let body = &pattern[1..close];
    let is_interval = !body.is_empty()
        && body.bytes().all(|b| b.is_ascii_digit() || b == b',')
        && body.bytes().any(|b| b.is_ascii_digit());
    is_interval.then_some(&pattern[close + 1..])
}

/// True when `pattern` holds a bracket expression that looks like a misspelled
/// character class, e.g. `[:space:]` instead of `[[:space:]]`. GNU grep rejects
/// those: a bracket whose first and last characters are colons, that holds at
/// least one other character, and that contains no range, class, equivalence
/// class or collating element.
fn has_confusing_bracket(pattern: &[u8]) -> bool {
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            b'\\' => i += 2,
            b'[' => {
                let (confusing, next) = scan_bracket(pattern, i + 1);
                if confusing {
                    return true;
                }
                i = next;
            }
            _ => i += 1,
        }
    }
    false
}

/// Scan the body of a bracket expression starting at `start` (just past the
/// `[`). Returns whether it is a misspelled character class and the index just
/// past its closing `]`.
fn scan_bracket(pattern: &[u8], start: usize) -> (bool, usize) {
    const FIRST_IS_COLON: u8 = 1;
    const LAST_IS_COLON: u8 = 2;
    const HAS_OTHER: u8 = 4;
    const HAS_RANGE_OR_CLASS: u8 = 8;

    let mut i = start;
    if pattern.get(i) == Some(&b'^') {
        i += 1;
    }
    let body_start = i;
    let mut state = if pattern.get(i) == Some(&b':') {
        FIRST_IS_COLON
    } else {
        0
    };
    while i < pattern.len() {
        let c = pattern[i];
        // A `]` right at the start of the body is an ordinary character.
        if c == b']' && i != body_start {
            return (state == FIRST_IS_COLON | LAST_IS_COLON | HAS_OTHER, i + 1);
        }
        // Only the character just before the closing `]` counts as the last one.
        state &= !LAST_IS_COLON;
        // `[:alpha:]`, `[.a.]` and `[=a=]` inside the bracket.
        if c == b'[' && matches!(pattern.get(i + 1), Some(b':' | b'.' | b'=')) {
            let delimiter = pattern[i + 1];
            if let Some(end) = find_bracket_subexpr_end(pattern, i + 2, delimiter) {
                state |= HAS_RANGE_OR_CLASS;
                i = end;
                continue;
            }
        }
        // `x-y` is a range, but the `-` of `[x-]` is an ordinary character.
        if pattern.get(i + 1) == Some(&b'-')
            && matches!(pattern.get(i + 2), Some(&other) if other != b']')
        {
            state |= HAS_RANGE_OR_CLASS;
            i += 3;
            continue;
        }
        state |= if c == b':' { LAST_IS_COLON } else { HAS_OTHER };
        i += 1;
    }
    // Unterminated bracket: the regex engine reports that on its own.
    (false, pattern.len())
}

/// Index just past the `:]`, `.]` or `=]` closing a `[: [. [=` subexpression
/// whose body starts at `start`.
fn find_bracket_subexpr_end(pattern: &[u8], start: usize, delimiter: u8) -> Option<usize> {
    (start..pattern.len().saturating_sub(1))
        .find(|&i| pattern[i] == delimiter && pattern[i + 1] == b']')
        .map(|i| i + 2)
}

#[cfg(test)]
mod tests {
    use super::{has_confusing_bracket, plain_literal};
    use crate::RegexMode;

    fn lit(p: &str, ic: bool, mode: RegexMode) -> Option<Vec<u8>> {
        plain_literal(p, ic, mode)
    }

    #[test]
    fn fixed_mode_takes_any_ascii_verbatim() {
        // Under -F every byte is literal, even regex metacharacters.
        assert_eq!(lit("abc", false, RegexMode::Fixed), Some(b"abc".to_vec()));
        assert_eq!(lit("a.*b", false, RegexMode::Fixed), Some(b"a.*b".to_vec()));
        assert_eq!(lit("a+b", false, RegexMode::Fixed), Some(b"a+b".to_vec()));
    }

    #[test]
    fn regex_modes_accept_metacharacter_free_literals() {
        for mode in [RegexMode::Basic, RegexMode::Extended, RegexMode::Perl] {
            assert_eq!(lit("ing", false, mode), Some(b"ing".to_vec()));
            assert_eq!(lit("Hello123", false, mode), Some(b"Hello123".to_vec()));
        }
    }

    #[test]
    fn regex_modes_reject_anything_with_a_metacharacter() {
        for mode in [RegexMode::Basic, RegexMode::Extended, RegexMode::Perl] {
            for p in [
                "a.b", "a*", "[ab]", "^a", "a$", "a\\b", "a+", "a?", "(a)", "a|b", "a{2}",
            ] {
                assert_eq!(lit(p, false, mode), None, "pattern {p:?} in {mode:?}");
            }
        }
    }

    #[test]
    fn rejects_empty_case_insensitive_and_non_ascii() {
        assert_eq!(lit("", false, RegexMode::Fixed), None);
        assert_eq!(lit("abc", true, RegexMode::Fixed), None); // -i
        assert_eq!(lit("abc", true, RegexMode::Basic), None);
        assert_eq!(lit("café", false, RegexMode::Fixed), None); // non-ASCII
        assert_eq!(lit("naïve", false, RegexMode::Basic), None);
    }

    #[test]
    fn detects_misspelled_character_classes() {
        for p in [
            "[:digit:]",
            "[^:digit:]",
            "q[:punct:]w",
            "[:notaclass:]",
            "[:x:]",
            "ab[:blank:]",
        ] {
            assert!(has_confusing_bracket(p.as_bytes()), "pattern {p:?}");
        }
    }

    #[test]
    fn accepts_bracket_expressions_that_are_not_confusing() {
        for p in [
            "[[:digit:]]",    // the correct spelling
            "[::]",           // no character besides the colons
            "[:digit]",       // does not end with a colon
            "[:digit:qrs]",   // ends with an ordinary character
            "[:dig-it:]",     // holds a range
            "[:x[:digit:]:]", // holds a character class
            "[:x[.,.]:]",     // holds a collating element
            "[:x[=e=]:]",     // holds an equivalence class
            "\\[:digit:]",    // the bracket is escaped
            "[]:digit:]",     // starts with a literal ']'
            "[:digit:",       // unterminated
            "[a-z]+[0-9]",    // no colons at all
        ] {
            assert!(!has_confusing_bracket(p.as_bytes()), "pattern {p:?}");
        }
    }

    #[test]
    fn test_has_invalid_char_class() {
        assert!(super::transpile_ere("[[:notdef:]]").is_err());
        assert!(super::transpile_ere("[[:digit:]]").is_ok());
        assert!(super::transpile_ere("[[:alpha:]]+").is_ok());
    }

    #[test]
    fn test_transpile_bre_basics() {
        assert_eq!(
            super::transpile_bre("a+b|c(d){e}").unwrap(),
            r"a\+b\|c\(d\)\{e\}"
        );
        assert_eq!(super::transpile_bre(r"o\+").unwrap(), "o+");
        assert_eq!(super::transpile_bre(r"Hi\|HI").unwrap(), "Hi|HI");
        assert_eq!(super::transpile_bre(r"a\{2,3\}").unwrap(), "a{2,3}");
        assert_eq!(super::transpile_bre(r"a\{,3\}").unwrap(), "a{0,3}");
        assert_eq!(
            super::transpile_bre(r"\(\b\w\+\b\) \1").unwrap(),
            r"(\b\w+\b) \1"
        );
        assert_eq!(super::transpile_bre("*foo").unwrap(), r"\*foo");
        assert_eq!(super::transpile_bre(r"\`c\|r\'").unwrap(), r"\Ac|r\z");
    }

    #[test]
    fn test_transpile_ere_alternations() {
        assert_eq!(
            super::transpile_ere("foo|foobar|foobarbaz").unwrap(),
            "foobarbaz|foobar|foo"
        );
        assert_eq!(super::transpile_ere(r"\`c|r\'").unwrap(), r"\Ac|r\z");
    }
}
