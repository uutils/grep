// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use crate::{Config, RegexMode};
use memchr::memmem;
use onig::{RegexOptions, Region, SearchOptions, Syntax, SyntaxBehavior, SyntaxOperator};
use onig_sys::{
    ONIGERR_EMPTY_RANGE_IN_CHAR_CLASS, OnigEncCtype_ONIGENC_CTYPE_WORD, OnigEncodingUTF8,
};
use std::borrow::Cow;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use uucore::error::{UResult, USimpleError};
use uucore::show_warning;

static ONIG_NEW_MUTEX: Mutex<()> = Mutex::new(());

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
    /// NOTE that `-w` does not check both sides, unlike `\b` in a regex.
    /// Start/End-of-line count as non-words.
    fn is_word_match(line: &[u8], start: usize, end: usize) -> bool {
        // SAFETY: This code uses OnigEncodingType such that it can support other types of encodings in the future.
        unsafe {
            let mbc_to_code = OnigEncodingUTF8.mbc_to_code.unwrap_unchecked();
            let is_code_ctype = OnigEncodingUTF8.is_code_ctype.unwrap_unchecked();
            let line_end = line.as_ptr().add(line.len());

            if end < line.len() {
                let cp = mbc_to_code(line.as_ptr().add(end), line_end);
                if is_code_ctype(cp, OnigEncCtype_ONIGENC_CTYPE_WORD) != 0 {
                    return false;
                }
            }

            if start > 0 {
                let left_adjust = OnigEncodingUTF8.left_adjust_char_head.unwrap_unchecked();
                let head = left_adjust(line.as_ptr(), line.as_ptr().add(start - 1));
                let cp = mbc_to_code(head, line_end);
                if is_code_ctype(cp, OnigEncCtype_ONIGENC_CTYPE_WORD) != 0 {
                    return false;
                }
            }

            true
        }
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
        let Some((start, leftmost_end)) = self.pattern.search_leftmost(self.line, self.offset)
        else {
            self.pending = None;
            return;
        };
        let end = self
            .pattern
            .longest_end_at(self.line, start)
            .unwrap_or(leftmost_end);
        // Advance the next search past the match we just found.
        // Zero-length matches need a +1 nudge to avoid spinning forever.
        self.offset = end.max(start + 1);
        self.pending = Some((start, end));
    }
}

/// Return the literal bytes of `pattern` when a raw byte-for-byte substring
/// search is *exactly* equivalent to matching it, otherwise `None`.
///
/// We accept only ASCII, case-sensitive needles. That keeps the byte search in
/// agreement with the regex engine on every possible input, including bytes that
/// are not valid UTF-8: an ASCII byte can never be part of a multi-byte sequence,
/// so its presence is unambiguous. In the regex modes we also require that no
/// byte could ever act as a metacharacter; under `-F` the text is literal as-is.
fn plain_literal(pattern: &str, ignore_case: bool, mode: RegexMode) -> Option<Vec<u8>> {
    if ignore_case || pattern.is_empty() || !pattern.is_ascii() {
        return None;
    }
    // Every byte that carries special meaning in any of our regex syntaxes.
    // A needle without these reads the same as a literal in Basic/Extended/Perl.
    const SPECIAL: &[u8] = b".*[]^$\\+?{}()|";
    let plain = mode == RegexMode::Fixed || !pattern.bytes().any(|b| SPECIAL.contains(&b));
    plain.then(|| pattern.as_bytes().to_vec())
}

struct CompiledPattern {
    /// Default semantics. It's decently fast and used for searching.
    leftmost: OnigRegex,
    /// Compiled with `FIND_LONGEST`. If used for a search, it'll search the
    /// entire haystack to find the longest. This makes it unsuitable for searching,
    /// but it's perfect for a second, anchored match pass for POSIX semantics.
    longest_anchored: OnigRegex,
}

impl CompiledPattern {
    fn compile(pattern: &str, config: &Config) -> UResult<Self> {
        let mut syntax = *match config.regex_mode {
            RegexMode::Fixed => Syntax::asis(),
            RegexMode::Basic => Syntax::grep(),
            RegexMode::Extended => Syntax::gnu_regex(),
            RegexMode::Perl => Syntax::perl_ng(),
        };
        if config.regex_mode != RegexMode::Fixed {
            // GNU grep supports `{,n}` as an alias for `{0,n}`.
            syntax.enable_behavior(SyntaxBehavior::SYNTAX_BEHAVIOR_ALLOW_INTERVAL_LOW_ABBREV);
        }
        if matches!(config.regex_mode, RegexMode::Basic | RegexMode::Extended) {
            // GNU grep supports \` and \' as buffer anchors in BRE and ERE.
            syntax.enable_operators(SyntaxOperator::SYNTAX_OPERATOR_ESC_GNU_BUF_ANCHOR);
        }

        if matches!(config.regex_mode, RegexMode::Basic | RegexMode::Extended)
            && has_confusing_bracket(pattern.as_bytes())
        {
            return Err(USimpleError::new(
                2,
                "character class syntax is [[:space:]], not [:space:]".to_string(),
            ));
        }

        let pattern = if config.regex_mode == RegexMode::Fixed {
            Cow::Borrowed(pattern)
        } else {
            rewrite_equivalence_classes(pattern)
        };
        let pattern: &str = &pattern;

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

        if config.regex_mode == RegexMode::Perl {
            // GNU grep supports `(?P<name>...)`.
            // Unfortunately, the onig crate defines the OP2 flag without the
            // necessary <<32 bit shift, so we need to hotpatch that here.
            const _: () =
                assert!(SyntaxOperator::SYNTAX_OPERATOR_QMARK_CAPITAL_P_NAME.bits() == 0x80000000);
            const FIXED: SyntaxOperator = SyntaxOperator::from_bits_retain(
                SyntaxOperator::SYNTAX_OPERATOR_QMARK_CAPITAL_P_NAME.bits() << 32,
            );
            syntax.enable_operators(FIXED);
        }

        let mut options = RegexOptions::REGEX_OPTION_NONE;
        if config.ignore_case {
            options |= RegexOptions::REGEX_OPTION_IGNORECASE;
        }
        // In GNU grep's Basic/Extended modes, `-z` makes newline ordinary data
        // for `.`, but PCRE keeps its existing non-DOTALL behavior. The GNU
        // `pcre-context` test documents this as current behavior until PCRE2.
        if config.null_data && matches!(config.regex_mode, RegexMode::Basic | RegexMode::Extended) {
            options |= RegexOptions::REGEX_OPTION_MULTILINE;
        }

        fn compile_with(
            pattern: &str,
            syntax: &Syntax,
            options: RegexOptions,
        ) -> UResult<OnigRegex> {
            OnigRegex::compile(pattern, syntax, options).map_err(|err| {
                // A reversed range like `[b-a]` is ONIGERR_EMPTY_RANGE_IN_CHAR_CLASS.
                // GNU grep reports it simply as "Invalid range end" (no pattern
                // echoed), so translate this code to match its diagnostic.
                let message = match err.code {
                    ONIGERR_EMPTY_RANGE_IN_CHAR_CLASS => "Invalid range end".to_string(),
                    _ => format!("invalid pattern \"{pattern}\": {}", err.message),
                };
                USimpleError::new(2, message)
            })
        }

        let leftmost = compile_with(pattern, &syntax, options)?;
        let longest_anchored = compile_with(
            pattern,
            &syntax,
            options | RegexOptions::REGEX_OPTION_FIND_LONGEST,
        )?;
        Ok(Self {
            leftmost,
            longest_anchored,
        })
    }

    /// Find the leftmost match starting at or after `offset`.
    fn search_leftmost(&self, line: &[u8], offset: usize) -> Option<(usize, usize)> {
        let mut region = Region::new();
        self.leftmost.search(line, offset, Some(&mut region))?;
        region.pos(0)
    }

    /// Given a known leftmost start `start`, return the longest extent
    /// of a match anchored exactly there = POSIX leftmost-longest end.
    fn longest_end_at(&self, line: &[u8], start: usize) -> Option<usize> {
        let mut region = Region::new();
        self.longest_anchored
            .match_at(line, start, Some(&mut region));
        region.pos(0).map(|(_, end)| end)
    }

    /// True if any match exists in `line` (including zero-length).
    fn is_match(&self, line: &[u8]) -> bool {
        self.leftmost.search(line, 0, None).is_some()
    }
}

struct OnigRegex {
    raw: onig_sys::OnigRegex,
}

// SAFETY: Oniguruma compiled regexes are immutable after construction, and this
// wrapper owns and frees the raw pointer exactly once. This mirrors `onig::Regex`.
unsafe impl Send for OnigRegex {}
// SAFETY: Searches only read the compiled regex. Capture storage is caller-owned
// through `Region`, so sharing the compiled regex across threads is safe.
unsafe impl Sync for OnigRegex {}

impl OnigRegex {
    fn compile(pattern: &str, syntax: &Syntax, options: RegexOptions) -> Result<Self, OnigError> {
        let pattern = pattern.as_bytes();
        let mut raw = null_mut();
        let mut error = onig_sys::OnigErrorInfo {
            enc: null_mut(),
            par: null_mut(),
            par_end: null_mut(),
        };
        // SAFETY: This reads Oniguruma's process default case-folding bitset.
        let mut case_fold_flag = unsafe { onig_sys::onig_get_default_case_fold_flag() };
        if options.contains(RegexOptions::REGEX_OPTION_IGNORECASE) {
            case_fold_flag &= !onig_sys::INTERNAL_ONIGENC_CASE_FOLD_MULTI_CHAR;
        }

        let mut compile_info = onig_sys::OnigCompileInfo {
            num_of_elements: 5,
            pattern_enc: &raw mut OnigEncodingUTF8,
            target_enc: &raw mut OnigEncodingUTF8,
            syntax: syntax as *const Syntax as *mut Syntax as *mut onig_sys::OnigSyntaxType,
            option: options.bits(),
            case_fold_flag,
        };

        let _guard = ONIG_NEW_MUTEX.lock().unwrap();
        // SAFETY: `pattern` supplies a valid start/end pointer pair for the
        // duration of the call, and `compile_info` uses Oniguruma's built-in
        // UTF-8 encoding plus a syntax value borrowed from the safe wrapper.
        let result = unsafe {
            onig_sys::onig_new_deluxe(
                &mut raw,
                pattern.as_ptr(),
                pattern.as_ptr().add(pattern.len()),
                &mut compile_info,
                &mut error,
            )
        };
        if result == onig_sys::ONIG_NORMAL as i32 {
            Ok(Self { raw })
        } else {
            Err(OnigError::new(result, &error))
        }
    }

    fn search(&self, line: &[u8], offset: usize, region: Option<&mut Region>) -> Option<usize> {
        debug_assert!(offset <= line.len());
        // SAFETY: `offset` is bounded by `line.len()`, all byte pointers are
        // derived from `line`, and `region_ptr` preserves `onig::Region`'s
        // transparent representation over `OnigRegion`.
        let result = unsafe {
            let start = line.as_ptr().add(offset);
            let end = line.as_ptr().add(line.len());
            onig_sys::onig_search(
                self.raw,
                line.as_ptr(),
                end,
                start,
                end,
                region_ptr(region),
                SearchOptions::SEARCH_OPTION_NONE.bits(),
            )
        };
        onig_match_result(result)
    }

    fn match_at(&self, line: &[u8], offset: usize, region: Option<&mut Region>) -> Option<usize> {
        debug_assert!(offset <= line.len());
        // SAFETY: `offset` is bounded by `line.len()`, all byte pointers are
        // derived from `line`, and `region_ptr` preserves `onig::Region`'s
        // transparent representation over `OnigRegion`.
        let result = unsafe {
            let at = line.as_ptr().add(offset);
            onig_sys::onig_match(
                self.raw,
                line.as_ptr(),
                line.as_ptr().add(line.len()),
                at,
                region_ptr(region),
                SearchOptions::SEARCH_OPTION_NONE.bits(),
            )
        };
        onig_match_result(result)
    }
}

impl Drop for OnigRegex {
    fn drop(&mut self) {
        // SAFETY: `raw` was returned by a successful `onig_new_deluxe` call and
        // is owned by this wrapper.
        unsafe { onig_sys::onig_free(self.raw) }
    }
}

struct OnigError {
    code: i32,
    message: String,
}

impl OnigError {
    fn new(code: i32, info: *const onig_sys::OnigErrorInfo) -> Self {
        Self {
            code,
            message: onig_error_message(code, info),
        }
    }
}

fn region_ptr(region: Option<&mut Region>) -> *mut onig_sys::OnigRegion {
    region.map_or(null_mut(), |r| {
        r as *mut Region as *mut onig_sys::OnigRegion
    })
}

fn onig_match_result(result: i32) -> Option<usize> {
    if result >= 0 {
        Some(result as usize)
    } else if result == onig_sys::ONIG_MISMATCH {
        None
    } else {
        panic!(
            "Onig: Regex match error: {}",
            onig_error_message(result, null())
        );
    }
}

fn onig_error_message(code: i32, info: *const onig_sys::OnigErrorInfo) -> String {
    let mut buff = [0; onig_sys::ONIG_MAX_ERROR_MESSAGE_LEN as usize];
    let len = unsafe { onig_sys::onig_error_code_to_str(buff.as_mut_ptr(), code, info) };
    String::from_utf8_lossy(&buff[..len as usize]).into_owned()
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
    while let Some(open) = next_unescaped(pattern, i, b'[') {
        let (confusing, next) = scan_bracket(pattern, open + 1);
        if confusing {
            return true;
        }
        i = next;
    }
    false
}

/// Index of the first `needle` at or after `from` that is not escaped by a
/// backslash, or `None` if the pattern holds no such byte.
///
/// Tracking the backslash run as we walk (rather than looking back from each
/// hit) keeps the scan linear, and one helper serves every "next significant
/// byte" search in a pattern: the opening `[` of a bracket expression, the
/// closing `]`, and the `:`/`.`/`=` that ends a `[:`/`[.`/`[=` subexpression.
/// An odd run of backslashes in front of a byte escapes it; an even one leaves
/// it literal (so `\\[` is an unescaped `[`).
fn next_unescaped(pattern: &[u8], mut from: usize, needle: u8) -> Option<usize> {
    let mut escape = false;
    while from < pattern.len() {
        match pattern[from] {
            b'\\' => escape = !escape,
            c if c == needle => {
                if !escape {
                    return Some(from);
                }
                escape = false;
            }
            _ => escape = false,
        }
        from += 1;
    }
    None
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
        if c == b'['
            && let Some(end) = bracket_subexpr_end(pattern, i)
        {
            state |= HAS_RANGE_OR_CLASS;
            i = end;
            continue;
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

/// Rewrite POSIX equivalence classes (`[=c=]`) to their bare member `c`.
///
/// A POSIX bracket expression may contain an equivalence class `[=c=]` that
/// matches every character collating equal to `c`. In the single-byte C locale
/// that set is just `c` itself, and oniguruma has no syntax for equivalence
/// classes at all, so a pattern containing one cannot be compiled as-is. This
/// function rewrites the classes oniguruma cannot express into the nearest
/// equivalent it can, leaving everything else in the pattern untouched:
///
/// * `[[=a=]]` becomes `[a]`        — the class collapses to its sole member.
/// * `[[=a=]b]` becomes `[ab]`      — members compose with other entries.
/// * `[[=a=][=b=]]` becomes `[ab]`  — several classes in one bracket.
/// * `x[[=a=]]y` becomes `x[a]y`    — text outside the bracket is preserved.
///
/// Returns a borrowed `Cow` when there is nothing to rewrite (the common case,
/// so no allocation is needed) and an owned one otherwise.
///
/// A class whose body is not exactly one character is left in place, as is one
/// used as a range endpoint (an error in GNU grep, so it must not be quietly
/// turned into a range) or one holding `]`, `^`, `-` or `\`, whose meaning
/// inside a bracket expression depends on where it sits.
fn rewrite_equivalence_classes(pattern: &str) -> Cow<'_, str> {
    let bytes = pattern.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut i = 0;

    while let Some(open) = next_unescaped(bytes, i, b'[') {
        let (body, next) = scan_equivalence_bracket(pattern, open);
        if let Cow::Owned(body) = body {
            out.push_str(&pattern[copied..=open]);
            out.push_str(&body);
            copied = next;
        }
        i = next;
    }

    if out.is_empty() {
        Cow::Borrowed(pattern)
    } else {
        out.push_str(&pattern[copied..]);
        Cow::Owned(out)
    }
}

/// Scan a single bracket expression whose opening `[` is at `open` and rewrite
/// every rewritable `[=c=]` equivalence class in its body to the bare member
/// `c`.
///
/// Returns the body — from just past the `[` through the closing `]` —
/// together with the index just past that `]`. The body is borrowed when the
/// bracket holds no rewritable class (the caller can leave the span untouched
/// and skip the allocation) and owned once at least one class was collapsed:
///
/// * `[[=a=]]`  scans to the borrowed span `=a=]`, then rewrites it to `a]`.
/// * `[[=a=]b]` keeps the trailing `b]` and rewrites to `ab]`.
///
/// Mirrors `scan_bracket`, which does the equivalent job for
/// `has_confusing_bracket`, so the two stay structurally alike.
fn scan_equivalence_bracket(pattern: &str, open: usize) -> (Cow<'_, str>, usize) {
    let bytes = pattern.as_bytes();
    // The body starts just past `[`. A leading `^` negates the bracket and a
    // `]` right after it (or after the `^`) is an ordinary member, so neither
    // can close the bracket; skip both before the scan begins.
    let mut j = open + 1;
    if bytes.get(j) == Some(&b'^') {
        j += 1;
    }
    let body_start = j;
    if bytes.get(j) == Some(&b']') {
        j += 1;
    }

    let mut out = String::new();
    let mut copied = open + 1; // first body byte not yet written into `out`
    let mut prev: Option<u8> = None; // the byte before `j`, for range-endpoint checks
    let mut escape = false;

    while let Some(&c) = bytes.get(j) {
        // An unescaped `]` that is not the leading literal one closes the bracket.
        if !escape && c == b']' && j != body_start {
            if out.is_empty() {
                return (Cow::Borrowed(&pattern[open + 1..=j]), j + 1);
            }
            out.push_str(&pattern[copied..=j]);
            return (Cow::Owned(out), j + 1);
        }
        // An unescaped `[` may open a `[:...]`, `[...]` or `[=...]` member. A
        // preceding backslash escapes it, so it is a literal, not a member start.
        if !escape
            && c == b'['
            && let Some(end) = bracket_subexpr_end(bytes, j)
        {
            // A rewritable `[=c=]` collapses to its sole member `c`; any other
            // member is left untouched. `prev` is the byte before the `[`, which
            // is what decides whether the class sits at a range endpoint.
            if bytes.get(j + 1) == Some(&b'=')
                && is_rewritable_equivalence(
                    &pattern[j + 2..end - 2],
                    bytes.get(end).copied(),
                    prev,
                )
            {
                out.push_str(&pattern[copied..j]);
                out.push_str(&pattern[j + 2..end - 2]);
                copied = end;
            }
            // A member always ends in `]`, so the next byte cannot be mid-range.
            prev = Some(b']');
            j = end;
            escape = false;
            continue;
        }
        escape = c == b'\\' && !escape;
        prev = Some(c);
        j += 1;
    }

    // Unterminated bracket: let the regex engine report it. Flush any rewrite
    // already started; otherwise lend the body back unchanged.
    if out.is_empty() {
        (Cow::Borrowed(&pattern[open + 1..]), bytes.len())
    } else {
        out.push_str(&pattern[copied..]);
        (Cow::Owned(out), bytes.len())
    }
}

/// True when a `[=...=]` equivalence class whose body is `body` can be replaced
/// by that single character. GNU grep rejects equivalence classes used as range
/// endpoints, so a class directly preceded or followed by `-` (`prev`/`next`)
/// is left in place rather than silently producing a range.
fn is_rewritable_equivalence(body: &str, next: Option<u8>, prev: Option<u8>) -> bool {
    let in_range = next == Some(b'-') || prev == Some(b'-');
    body.chars().count() == 1 && !body.starts_with([']', '^', '-', '\\']) && !in_range
}

/// If a `[:`, `[.` or `[=` subexpression starts at `start` (the `[`), return the
/// index just past its closing `:]`/`.]`/`=]`. The closing delimiter must not be
/// escaped, so a `\:` (or `\.`/`\=`) in the body does not end the subexpression.
/// Both `scan_bracket` and `scan_equivalence_bracket` share this helper so the
/// two stay consistent.
fn bracket_subexpr_end(pattern: &[u8], start: usize) -> Option<usize> {
    let delimiter = *pattern.get(start + 1)?;
    if !matches!(delimiter, b':' | b'.' | b'=') {
        return None;
    }
    let mut from = start + 2;
    loop {
        let at = next_unescaped(pattern, from, delimiter)?;
        if pattern.get(at + 1) == Some(&b']') {
            return Some(at + 2);
        }
        from = at + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{has_confusing_bracket, plain_literal, rewrite_equivalence_classes};
    use crate::RegexMode;
    use std::borrow::Cow;

    fn r(p: &str) -> Cow<'_, str> {
        rewrite_equivalence_classes(p)
    }

    #[test]
    fn equivalence_classes_reduce_to_their_member() {
        // Rewrites allocate an owned string.
        assert_eq!(&*r("[[=a=]]"), "[a]");
        assert_eq!(&*r("[[=a=]b]"), "[ab]");
        assert_eq!(&*r("[b[=a=]]"), "[ba]");
        assert_eq!(&*r("[[=a=][=b=]]"), "[ab]");
        assert_eq!(&*r("[^[=a=]]"), "[^a]");
        assert_eq!(&*r("x[[=a=]]y"), "x[a]y");
        assert_eq!(&*r("[[:alpha:][=a=]]"), "[[:alpha:]a]");
        // The common case borrows the input unchanged: no allocation.
        assert!(matches!(r("abc"), Cow::Borrowed("abc")));
    }

    #[test]
    fn equivalence_class_rewrite_leaves_other_patterns_alone() {
        // Nothing to do.
        assert!(matches!(r("abc"), Cow::Borrowed(_)));
        assert!(matches!(r("[abc]"), Cow::Borrowed(_)));
        assert!(matches!(r("[[:alpha:]]"), Cow::Borrowed(_)));
        assert!(matches!(r("[[.a.]]"), Cow::Borrowed(_)));
        // Not a bracket expression: `[=a=]` outside `[...]` is literal in GNU.
        assert!(matches!(r("\\[[=a=]"), Cow::Borrowed(_)));
        // A range endpoint is an error in GNU; don't invent a valid range.
        assert!(matches!(r("[[=a=]-c]"), Cow::Borrowed(_)));
        assert!(matches!(r("[a-[=c=]]"), Cow::Borrowed(_)));
        // Only single-character classes have an obvious C-locale member.
        assert!(matches!(r("[[=ab=]]"), Cow::Borrowed(_)));
        assert!(matches!(r("[[==]]"), Cow::Borrowed(_)));
        // Members whose meaning depends on position inside the bracket.
        assert!(matches!(r("[[=]=]]"), Cow::Borrowed(_)));
        assert!(matches!(r("[[=^=]]"), Cow::Borrowed(_)));
        assert!(matches!(r("[[=-=]]"), Cow::Borrowed(_)));
    }

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
            "\\\\[:blank:]", // the backslash is escaped, so the bracket is not
        ] {
            assert!(has_confusing_bracket(p.as_bytes()), "pattern {p:?}");
        }
    }

    #[test]
    fn accepts_bracket_expressions_that_are_not_confusing() {
        for p in [
            "[[:digit:]]",     // the correct spelling
            "[::]",            // no character besides the colons
            "[:digit]",        // does not end with a colon
            "[:digit:qrs]",    // ends with an ordinary character
            "[:dig-it:]",      // holds a range
            "[:x[:digit:]:]",  // holds a character class
            "[:x[.,.]:]",      // holds a collating element
            "[:x[=e=]:]",      // holds an equivalence class
            "\\[:digit:]",     // the bracket is escaped
            "\\\\\\[:digit:]", // and still escaped after an escaped backslash
            "[]:digit:]",      // starts with a literal ']'
            "[:digit:",        // unterminated
            "[a-z]+[0-9]",     // no colons at all
        ] {
            assert!(!has_confusing_bracket(p.as_bytes()), "pattern {p:?}");
        }
    }
}
