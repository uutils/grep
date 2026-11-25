use std::sync::atomic::{AtomicBool, Ordering};

use rstest::rstest;
#[cfg(unix)]
use uutests::util::TerminalSimulation;
use uutests::util::{TestScenario, UCommand};

static UCMD_INIT: AtomicBool = AtomicBool::new(false);

fn ucmd() -> (TestScenario, UCommand) {
    if !UCMD_INIT.swap(true, Ordering::Relaxed) {
        unsafe { std::env::set_var("UUTESTS_BINARY_PATH", env!("CARGO_BIN_EXE_grep")) };
    }

    let scene = TestScenario::new("grep");
    let cmd = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    (scene, cmd)
}

// BRE: . * ^ $ [] [^] and literal +, |, (, )
#[rstest]
#[case("a.c", "abc\nadc\nac\n", "abc\nadc\n")]
#[case("fo*", "f\nfo\nfoo\nbar\n", "f\nfo\nfoo\n")]
#[case("^foo", "foo\nbarfoo\nfoo\n", "foo\nfoo\n")]
#[case("bar$", "bar\nfoobar\nbarx\n", "bar\nfoobar\n")]
#[case("[Hh]i", "Hi\nhi\nHI\n", "Hi\nhi\n")]
#[case("[^a-z]X", "aX\nbX\n.X\n", ".X\n")]
// `+`, `|`, `(`, `)` are literals in BRE
#[case("a+b", "a+b\nab\n", "a+b\n")]
#[case("a|b", "a|b\na\n", "a|b\n")]
#[case("(x)", "(x)\nx\n", "(x)\n")]
fn bre_default_metacharacters(#[case] pattern: &str, #[case] input: &str, #[case] expected: &str) {
    let (_s, mut c) = ucmd();
    c.args(&[pattern])
        .pipe_in(input)
        .succeeds()
        .stdout_only(expected);
}

// \+ \? \| \{m,n\} \< \> \b \w plus backreferences and leading `*`.
#[rstest]
#[case(r"o\+", "o\noo\nx\n", "o\noo\n")]
#[case(r"Hi\|HI", "Hi\nHI\nhi\n", "Hi\nHI\n")]
#[case(r"a\{2,3\}", "a\naa\naaaa\n", "aa\naaaa\n")]
#[case(r"\<word\>", "word\nwording\nthe word here\n", "word\nthe word here\n")]
#[case(r"\bcontain\b", "contain\ncontainer\ncontained\n", "contain\n")]
// BRE backreference: repeated adjacent word.
#[case(
    r"\(\b\w\+\b\) \1",
    "the the cat\nfoo bar\nis is great\n",
    "the the cat\nis is great\n"
)]
// Leading `*` is literal in BRE.
#[case("*foo", "*foo\nfoo\n**foo\n", "*foo\n**foo\n")]
fn bre_gnu_extensions(#[case] pattern: &str, #[case] input: &str, #[case] expected: &str) {
    let (_s, mut c) = ucmd();
    c.args(&[pattern])
        .pipe_in(input)
        .succeeds()
        .stdout_only(expected);
}

#[rstest]
#[case::basic(&[r"\`c\|r\'"])]
#[case::extended(&["-E", r"\`c|r\'"])]
fn gnu_buffer_anchors(#[case] args: &[&str]) {
    let (_s, mut c) = ucmd();
    c.args(args)
        .pipe_in("cat\nscat\ntar\ndog\n")
        .succeeds()
        .stdout_only("cat\ntar\n");
}

#[rstest]
#[case(&["-E", "Hi|HI"], "Hi\nHI\nhi\n", "Hi\nHI\n")]
#[case(&["-E", "o+"], "o\noo\nx\n", "o\noo\n")]
#[case(&["-E", "colou?r"], "colour\ncolor\ncolouur\n", "colour\ncolor\n")]
#[case(&["-E", "(foo|bar)x"], "foox\nbarx\nbaz\n", "foox\nbarx\n")]
#[case(&["-E", "o{2}"], "o\noo\nooo\n", "oo\nooo\n")]
#[case(&["-E", "a{,2}b"], "b\nab\naab\naaab\n", "b\nab\naab\naaab\n")]
// ERE backreference works on the Oniguruma path.
#[case(&["-E", r"(....).*\1"], "beriberi\nhelloworld\n", "beriberi\n")]
fn ere_metacharacters(#[case] args: &[&str], #[case] input: &str, #[case] expected: &str) {
    let (_s, mut c) = ucmd();
    c.args(args).pipe_in(input).succeeds().stdout_only(expected);
}

#[test]
fn ere_invalid_pattern_is_error() {
    let (_s, mut c) = ucmd();
    c.args(&["-E", "["])
        .fails_with_code(2)
        .stderr_contains("invalid pattern");
}

#[test]
fn ere_unmatched_close_paren_is_literal() {
    let (_s, mut c) = ucmd();
    c.args(&["-E", "a)"])
        .pipe_in("a)\nb\n")
        .succeeds()
        .stdout_only("a)\n");
}

// A reversed bracket range like `[b-a]` must fail with exit 2 and GNU's
// diagnostic "Invalid range end", in both basic and extended modes.
#[rstest]
#[case::basic(&["[b-a]"])]
#[case::extended(&["-E", "[b-a]"])]
fn reversed_range_endpoints_match_gnu_message(#[case] args: &[&str]) {
    let (_s, mut c) = ucmd();
    c.args(args)
        .fails_with_code(2)
        .stderr_contains("Invalid range end");
}

#[test]
fn unreadable_pattern_file_is_error() {
    // An unreadable -f / --exclude-from file is a usage error (exit 2), not the
    // exit 1 that means "no match". Only the label is ours: the text after it
    // is the operating system's, so match it on Unix alone (Windows says
    // "Access is denied." for a directory).
    #[cfg(unix)]
    const DIRECTORY: &str = "adir: Is a directory";
    #[cfg(not(unix))]
    const DIRECTORY: &str = "adir: ";
    #[cfg(unix)]
    const MISSING: &str = "no-such-pattern-file: No such file or directory";
    #[cfg(not(unix))]
    const MISSING: &str = "no-such-pattern-file: ";

    // The pattern file is read before any input, so grep exits without draining
    // stdin and the harness's write can lose the race with EPIPE.
    let (scenario, mut c) = ucmd();
    scenario.fixtures.mkdir("adir");
    c.args(&["-f", "adir"])
        .pipe_in("hello\n")
        .ignore_stdin_write_error()
        .fails_with_code(2)
        .stderr_contains(DIRECTORY);

    let (_s, mut c) = ucmd();
    c.args(&["-f", "no-such-pattern-file"])
        .pipe_in("hello\n")
        .ignore_stdin_write_error()
        .fails_with_code(2)
        .stderr_contains(MISSING);

    let (_s, mut c) = ucmd();
    c.args(&["--exclude-from=no-such-pattern-file", "hello"])
        .pipe_in("hello\n")
        .ignore_stdin_write_error()
        .fails_with_code(2)
        .stderr_contains(MISSING);
}

// `[:digit:]` is almost certainly a typo for `[[:digit:]]`; GNU rejects it
// with exit 2 in the basic and extended syntaxes.
#[rstest]
#[case(&["[:digit:]"])]
#[case(&["-E", "q[^:digit:]w"])]
#[case(&["-E", "[:nosuchclass:]"])]
fn misspelled_character_class_is_error(#[case] args: &[&str]) {
    let (_s, mut c) = ucmd();
    c.args(args)
        .fails_with_code(2)
        .stderr_contains("character class syntax is [[:space:]], not [:space:]");
}

// Bracket expressions that only look like a misspelled class stay valid.
#[rstest]
#[case(&["[[:digit:]]"])]
#[case(&["[::]"])]
#[case(&["-E", "[:digit]"])]
#[case(&["-E", "[:dig-it:]"])]
#[case(&["-F", "[:digit:]"])]
fn character_class_lookalike_is_valid(#[case] args: &[&str]) {
    let (_s, mut c) = ucmd();
    c.args(args).pipe_in("w[:digit:]7d\n").succeeds();
}

#[test]
fn max_count_rewinds_seekable_stdin() {
    // -m stops mid-input after reading ahead. When standard input can seek,
    // the unread remainder must be left in place for the next reader.
    use std::fs::File;
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    let (scenario, _) = ucmd();
    scenario.fixtures.write("lines", "a\nq\nb\nq\nc\n");
    let mut input = File::open(scenario.fixtures.plus("lines")).unwrap();

    // A dup of the descriptor shares its file offset with the child.
    let status = Command::new(env!("CARGO_BIN_EXE_grep"))
        .args(["-m1", "q"])
        .stdin(Stdio::from(input.try_clone().unwrap()))
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());

    let mut rest = String::new();
    input.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "b\nq\nc\n");
}

// Once -m is satisfied grep must exit without waiting for more input, even
// though standard input is still open (as with `tail -f log | grep -m1 x`).
#[cfg(unix)]
#[rstest]
#[case(&["-m1", "word"], "word\n")]
#[case(&["-c", "-m1", "word"], "1\n")]
#[case(&["-m2", "word"], "word\nword\n")]
fn max_count_literal_stops_reading_open_pipe(#[case] args: &[&str], #[case] expected: &str) {
    let (_s, mut c) = ucmd();
    let mut child = c
        .args(args)
        .set_stdin(std::process::Stdio::piped())
        .run_no_wait();
    // Write the lines but keep stdin open.
    child.write_in("word\nother\nword\nother\n");
    child.delay(500);
    let still_running = child.is_alive();
    child.close_stdin(); // let a buggy grep finish so the test cannot hang
    let result = child.wait().unwrap();
    assert!(
        !still_running,
        "grep {args:?} kept reading after the -m limit"
    );
    result.success().stdout_only(expected);
}

#[test]
fn max_count_literal_still_detects_binary_in_read_chunk() {
    // The chunk holding the N-th match is still scanned for NUL, so a NUL after
    // it gives the binary notice, the same as without another match before it.
    let (_s, mut c) = ucmd();
    c.args(&["-m1", "word"])
        .pipe_in("word\nword\nfoo\0\n")
        .succeeds()
        .stdout_is("word\n")
        .stderr_contains("binary file matches");
}

#[test]
fn quiet_match_overrides_file_error() {
    // With -q, a match makes grep exit 0 even if an earlier file could not be
    // opened. Without -q the missing file still yields exit 2, and -q with no
    // match keeps the error status.
    // -q still reports a file it cannot read, but files after the first match
    // are never opened. -qs suppresses the report; the status is still 2.
    #[cfg(not(windows))]
    let expected = "grep: no-such-file: No such file or directory\n";
    #[cfg(windows)]
    let expected = "grep: no-such-file: The system cannot find the file specified.\n";
    let (_s, mut c) = ucmd();
    c.args(&["-q", "abc", "no-such-file", "-"])
        // -q may exit before the harness finishes writing stdin (EPIPE).
        .ignore_stdin_write_error()
        .pipe_in("abcd\n")
        .succeeds()
        .no_stdout()
        .stderr_is(expected);

    let (_s, mut c) = ucmd();
    c.args(&["abc", "no-such-file", "-"])
        .pipe_in("abcd\n")
        .fails_with_code(2);

    let (_s, mut c) = ucmd();
    c.args(&["-q", "zzz", "no-such-file", "-"])
        .pipe_in("abcd\n")
        .fails_with_code(2)
        .no_stdout()
        .stderr_is(expected);

    let (scene, mut c) = ucmd();
    scene.fixtures.write("hit", "x\n");
    c.args(&["-q", "x", "hit", "no-such-file"])
        .succeeds()
        .no_output();

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-qs", "x", "no-such-file"])
        .fails_with_code(2)
        .no_output();

    #[cfg(unix)]
    {
        let (scene, mut c) = ucmd();
        scene.fixtures.mkdir("dir");
        c.args(&["-q", "x", "dir"])
            .fails_with_code(2)
            .no_stdout()
            .stderr_is("grep: dir: Is a directory\n");
    }
}

#[test]
fn initial_tab_skips_empty_lines() {
    // -T aligns content with a tab, but GNU omits the tab for an empty line
    // (a whitespace-only line still gets one). -H forces the filename prefix
    // on, so the tab is exercised.
    let (s, mut c) = ucmd();
    s.fixtures.write("in", "x\n\n");
    c.args(&["-T", "-H", "^", "in"])
        .succeeds()
        .stdout_is("in:\tx\nin:\n");

    let (s, mut c) = ucmd();
    s.fixtures.write("in", "x\n \n");
    c.args(&["-T", "-H", "^", "in"])
        .succeeds()
        .stdout_is("in:\tx\nin:\t \n");
}

#[rstest]
#[case("?", "warning: ? at start of expression")]
#[case("*", "warning: * at start of expression")]
#[case("+", "warning: + at start of expression")]
#[case("{2}", "warning: {...} at start of expression")]
#[case("{,2}", "warning: {...} at start of expression")]
fn ere_leading_repeat_operators_warn_and_match_empty(#[case] pattern: &str, #[case] warning: &str) {
    let (_s, mut c) = ucmd();
    c.args(&["-E", "-e", pattern])
        .pipe_in("abc\n")
        .succeeds()
        .stdout_is("abc\n")
        .stderr_contains(warning);
}

#[test]
fn bre_leading_star_is_literal() {
    let (_s, mut c) = ucmd();
    c.args(&["*foo"])
        .pipe_in("*foo\nfoo\n")
        .succeeds()
        .stdout_only("*foo\n");
}

#[test]
fn fixed_string_is_literal() {
    // Metacharacters are not interpreted.
    let (_s, mut c) = ucmd();
    c.args(&["-F", ".*+?"])
        .pipe_in("a.*+?b\n.*\n")
        .succeeds()
        .stdout_only("a.*+?b\n");

    // `o+` is the two characters `o+`, so doesn't match `foo`.
    let (_s, mut c) = ucmd();
    c.args(&["-F", "o+"]).pipe_in("foo\n").fails_with_code(1);

    // Multiple -e patterns.
    let (_s, mut c) = ucmd();
    c.args(&["-F", "-e", "hi", "-e", "HI"])
        .pipe_in("hi\nHI\nlo\n")
        .succeeds()
        .stdout_only("hi\nHI\n");
}

#[rstest]
#[case(&["-e", ".", "-F", "-G"])]
#[case(&["-e", ".", "-F", "-E"])]
#[case(&["-e", ".", "-E", "-G"])]
#[case(&["-e", ".", "-G", "-E"])]
#[case(&["-e", ".", "-P", "-F"])]
#[case(&["-e", ".", "-P", "-E"])]
#[case(&["-e", ".", "-P", "-G"])]
#[case(&["-e", ".", "-G", "-F", "-E"])]
#[case(&["-e", ".", "--fixed-strings", "--basic-regexp"])]
fn conflicting_matcher_flags_are_rejected(#[case] args: &[&str]) {
    let (_s, mut c) = ucmd();
    c.args(args)
        .fails_with_code(2)
        .stderr_contains("conflicting matchers specified")
        .no_stdout();
}

#[test]
fn repeated_matcher_flag_is_accepted() {
    let (_s, mut c) = ucmd();
    c.args(&["-e", ".", "-F", "-F"])
        .pipe_in("abc\n")
        .fails_with_code(1)
        .no_stdout();

    let (_s, mut c) = ucmd();
    c.args(&["-e", ".", "-E", "-E"])
        .pipe_in("abc\n")
        .succeeds()
        .stdout_only("abc\n");
}

#[test]
fn pcre_features() {
    let (_s, mut c) = ucmd();
    c.args(&["-P", r"\d+"])
        .pipe_in("abc\n123\nfoo42bar\n")
        .succeeds()
        .stdout_only("123\nfoo42bar\n");

    // Lookahead.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-o", r"foo(?=\d)"])
        .pipe_in("foo123\nfooBar\n")
        .succeeds()
        .stdout_only("foo\n");

    // Lookbehind.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-o", r"(?<=v=)\d+"])
        .pipe_in("v=42\nv=7x\n")
        .succeeds()
        .stdout_only("42\n7\n");
}

#[test]
fn perl_regexp_rejects_multiple_patterns() {
    // GNU grep's PCRE backend (-P) only supports a single pattern.
    // Multiple -e patterns must produce exit 2 and the canonical error message.
    // See: https://github.com/uutils/grep/issues/34

    // Two separate -e flags.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-e", "foo", "-e", "bar"])
        .fails_with_code(2)
        .stderr_contains("the -P option only supports a single pattern");

    // A newline inside the pattern string is split into multiple patterns.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-e", "foo\nbar"])
        .fails_with_code(2)
        .stderr_contains("the -P option only supports a single pattern");

    // A single pattern with -P must still work normally.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-e", r"\d+"])
        .pipe_in("abc\n42\n")
        .succeeds()
        .stdout_only("42\n");
}

#[test]
fn perl_regexp_duplicate_patterns() {
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-e", "b.r", "-e", "b.r"])
        .pipe_in("bar\nbaz\n")
        .succeeds()
        .stdout_only("bar\n");

    let (scene, mut c) = ucmd();
    scene.fixtures.write("same", "b.r\nb.r\n");
    c.args(&["-P", "-f", "same"])
        .pipe_in("bar\nbaz\n")
        .succeeds()
        .stdout_only("bar\n");

    // Different patterns are still refused. grep exits before reading its
    // input, so the harness's write to stdin can fail with EPIPE.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-e", "bar", "-e", "baz"])
        .pipe_in("bar\n")
        .ignore_stdin_write_error()
        .fails_with_code(2)
        .stderr_contains("only supports a single pattern");
}

#[test]
fn posix_character_classes() {
    let (_s, mut c) = ucmd();
    c.args(&["-E", "[[:digit:]]+"])
        .pipe_in("abc\n42\nx9y\n")
        .succeeds()
        .stdout_only("42\nx9y\n");

    let (_s, mut c) = ucmd();
    c.args(&["-E", "[[:notdef:]]"]).fails_with_code(2);
}

#[test]
fn longest_match_semantics() {
    // -F with overlapping alternatives must return the longest.
    let (_s, mut c) = ucmd();
    c.args(&["-F", "-o", "-e", "sam", "-e", "samwise"])
        .pipe_in("samwise\n")
        .succeeds()
        .stdout_only("samwise\n");

    // ERE alternation: longest wins, regardless of branch order.
    let (_s, mut c) = ucmd();
    c.args(&["-E", "-o", "foo|foobar|foobarbaz"])
        .pipe_in("foobarbaz\n")
        .succeeds()
        .stdout_only("foobarbaz\n");

    // Regression: `REGEX_OPTION_FIND_LONGEST` must be re-anchored at each
    // match start. A naive line-anchored search would swallow `"x" "y"`
    // as a single match.
    let (_s, mut c) = ucmd();
    c.args(&["-o", r#""[^"]*""#])
        .pipe_in("\"x\"  \"y\"\n")
        .succeeds()
        .stdout_only("\"x\"\n\"y\"\n");
}

#[test]
fn ignore_case_and_override() {
    let input = "Hello\nhELLO\nHELLO\nworld\n";

    // -i: all three case variants match the lowercase pattern.
    let (_s, mut c) = ucmd();
    c.args(&["-i", "hello"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("Hello\nhELLO\nHELLO\n");

    // --no-ignore-case undoes an earlier -i. Searching for `Hello` now
    // matches only the exactly-cased line.
    let (_s, mut c) = ucmd();
    c.args(&["-i", "--no-ignore-case", "Hello"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("Hello\n");
}

#[test]
fn ignore_case_does_not_expand_single_atom_to_multiple_chars() {
    let (_s, mut c) = ucmd();
    c.args(&["-o", "-i", "[[:alpha:]]"])
        .pipe_in("st\nss\nffi\n")
        .succeeds()
        .stdout_only("s\nt\ns\ns\nf\nf\ni\n");

    let (_s, mut c) = ucmd();
    c.args(&["-o", "-i", "ß"])
        .pipe_in("SS\n")
        .fails_with_code(1)
        .no_output();
}

#[test]
fn invert_match() {
    let (_s, mut c) = ucmd();
    c.args(&["-v", "foo"])
        .pipe_in("foo\nbar\nfoobar\nbaz\n")
        .succeeds()
        .stdout_only("bar\nbaz\n");
}

#[test]
fn word_regexp() {
    let (_s, mut c) = ucmd();
    c.args(&["-w", "foo"])
        .pipe_in("foo\nfoobar\nfoo bar\nxfoox\n")
        .succeeds()
        .stdout_only("foo\nfoo bar\n");

    // Also works with -F.
    let (_s, mut c) = ucmd();
    c.args(&["-F", "-w", "foo"])
        .pipe_in("foo bar\nfoobar\n")
        .succeeds()
        .stdout_only("foo bar\n");

    let (_s, mut c) = ucmd();
    c.args(&["-w", "$"])
        .pipe_in("abc\n\nx\n")
        .succeeds()
        .stdout_only("\n");
}

#[test]
fn line_regexp() {
    let (_s, mut c) = ucmd();
    c.args(&["-x", "foo bar"])
        .pipe_in("foo bar\nfoo bar!\nx foo bar\n")
        .succeeds()
        .stdout_only("foo bar\n");

    let (_s, mut c) = ucmd();
    c.args(&["-x", "$"])
        .pipe_in("abc\n\nx\n")
        .succeeds()
        .stdout_only("\n");
}

#[test]
fn max_count() {
    // -m makes grep stop before draining stdin, so the harness's write may fail
    // with EPIPE. That is expected and must not fail the test.

    // Basic cap.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "2", "a"])
        .ignore_stdin_write_error()
        .pipe_in("a\na\na\na\n")
        .succeeds()
        .stdout_only("a\na\n");

    // -m 0 means zero. Exit 1, no output.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "0", "a"])
        .ignore_stdin_write_error()
        .pipe_in("a\n")
        .fails_with_code(1)
        .no_stdout();

    let (_s, mut c) = ucmd();
    c.args(&["-m", "0", "a", "missing"])
        .fails_with_code(1)
        .no_output();

    let (_s, mut c) = ucmd();
    c.args(&["-m", "1", "a", "missing"])
        .fails_with_code(2)
        .stderr_contains("grep: missing:");

    let (_s, mut c) = ucmd();
    c.args(&["-m", "0", "-e", "["])
        .fails_with_code(2)
        .stderr_contains("invalid pattern");

    // -A trailing context still printed after the cutoff.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "1", "-A", "2", "match"])
        .ignore_stdin_write_error()
        .pipe_in("noise\nmatch\nctx1\nctx2\ntail\n")
        .succeeds()
        .stdout_only("match\nctx1\nctx2\n");

    // -c is capped by -m.
    let (_s, mut c) = ucmd();
    c.args(&["-c", "-m", "1", "a"])
        .ignore_stdin_write_error()
        .pipe_in("a\na\na\n")
        .succeeds()
        .stdout_only("1\n");
}

#[test]
fn pattern_sources() {
    // Positional.
    let (_s, mut c) = ucmd();
    c.args(&["foo"])
        .pipe_in("foo\nbar\n")
        .succeeds()
        .stdout_only("foo\n");

    // -e single and repeated.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "foo", "-e", "bar"])
        .pipe_in("foo\nbar\nbaz\n")
        .succeeds()
        .stdout_only("foo\nbar\n");

    // -e containing a newline is split into multiple patterns.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "foo\nbar"])
        .pipe_in("foo\nbaz\nbar\n")
        .succeeds()
        .stdout_only("foo\nbar\n");

    // -f reads one pattern per line.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("pats", "foo\nbar\n");
    c.args(&["-f", "pats"])
        .pipe_in("foo\nbaz\nbar\n")
        .succeeds()
        .stdout_only("foo\nbar\n");

    // Combined -e and -f.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("pats", "bar\n");
    c.args(&["-e", "foo", "-f", "pats"])
        .pipe_in("foo\nbar\nbaz\n")
        .succeeds()
        .stdout_only("foo\nbar\n");

    // -f from stdin via `-`.
    let (_s, mut c) = ucmd();
    c.args(&["-f", "-", "-e", "literal"])
        .pipe_in("foo\nbar\n")
        .fails_with_code(1);

    // `-e EXPR` splits on `\n`; trailing newline = empty pattern = matches all.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "foo\n"])
        .pipe_in("foo\nbar\nbaz\n")
        .succeeds()
        .stdout_only("foo\nbar\nbaz\n");

    // `-f` strips one trailing `\n` as the file terminator, so a sole-`\n`
    // file is one empty pattern (vs. zero for a truly empty file).
    let (scene, mut c) = ucmd();
    scene.fixtures.write("pat_just_nl", "\n");
    c.args(&["-f", "pat_just_nl"])
        .pipe_in("a\nb\n")
        .succeeds()
        .stdout_only("a\nb\n");
}

#[rstest]
fn empty_pattern_file_does_not_open_input(#[values("-G", "-E", "-F")] syntax: &str) {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    c.args(&[syntax, "-f", "empty", "no-such-file"])
        .fails_with_code(1)
        .no_output();
}

#[test]
fn empty_pattern_file_matches_nothing() {
    // An empty pattern list matches no line, so grep exits without reading its
    // inputs; the harness's write to stdin races grep's exit and may fail with EPIPE.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    c.args(&["-f", "empty"])
        .ignore_stdin_write_error()
        .pipe_in("anything\n")
        .fails_with_code(1)
        .no_stdout();

    // No input is opened: a missing file is not reported and `-c` prints nothing.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    scene.fixtures.write("birds.txt", "crow\n");
    c.args(&["-c", "-f", "empty", "birds.txt", "gone.txt"])
        .fails_with_code(1)
        .no_output();

    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    c.args(&["-c", "-f", "empty"])
        .ignore_stdin_write_error()
        .pipe_in("crow\n")
        .fails_with_code(1)
        .no_output();

    // `-v` selects every line and `-L` lists every file, so both still read.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    scene.fixtures.write("birds.txt", "crow\nwren\n");
    c.args(&["-v", "-f", "empty", "birds.txt"])
        .succeeds()
        .stdout_is("crow\nwren\n");

    // Exit status follows match semantics, like `-L missing`.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    scene.fixtures.write("a.txt", "crow\n");
    scene.fixtures.write("b.txt", "wren\n");
    c.args(&["-L", "-f", "empty", "a.txt", "b.txt"])
        .fails_with_code(1)
        .stdout_is("a.txt\nb.txt\n");

    // A file with one empty line is not an empty list: it matches every line.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty-line", "\n");
    scene.fixtures.write("birds.txt", "crow\nwren\n");
    c.args(&["-c", "-f", "empty-line", "birds.txt"])
        .succeeds()
        .stdout_is("2\n");

    // Another pattern source still searches.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    scene.fixtures.write("birds.txt", "crow\nwren\n");
    c.args(&["-f", "empty", "-e", "wren", "birds.txt"])
        .succeeds()
        .stdout_is("wren\n");
}

#[test]
fn empty_pattern_matches_every_line() {
    let (_s, mut c) = ucmd();
    c.args(&["-E", ""])
        .pipe_in("a\nb\nc\n")
        .succeeds()
        .stdout_only("a\nb\nc\n");
}

#[test]
fn inverted_empty_pattern_short_circuits() {
    // grep short-circuits without reading stdin at all, so the harness's write
    // races grep's exit and may fail with EPIPE.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "", "-v", "-c"])
        .ignore_stdin_write_error()
        .pipe_in("a\nb\n")
        .fails_with_code(1)
        .no_output();

    let (_s, mut c) = ucmd();
    c.args(&["-e", "", "-v", "-c", "missing"])
        .fails_with_code(1)
        .no_output();

    // Pattern compilation fails before any input is read.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "", "-e", "[", "-v"])
        .ignore_stdin_write_error()
        .pipe_in("a\n")
        .fails_with_code(2)
        .no_stdout()
        .stderr_contains("invalid pattern");
}

#[test]
fn pattern_starting_with_dash_needs_double_dash() {
    let (_s, mut c) = ucmd();
    c.args(&["--", "-foo-"])
        .pipe_in("x -foo- y\nplain\n")
        .succeeds()
        .stdout_only("x -foo- y\n");

    // Or via -e.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "-foo-"])
        .pipe_in("x -foo- y\n")
        .succeeds()
        .stdout_only("x -foo- y\n");
}

#[test]
fn no_pattern_is_usage_error() {
    let (_s, mut c) = ucmd();
    c.fails_with_code(2);
}

#[test]
fn count_modes() {
    // Count for single stdin.
    let (_s, mut c) = ucmd();
    c.args(&["-c", "a"])
        .pipe_in("a\nb\na\n")
        .succeeds()
        .stdout_only("2\n");

    // Count with -v counts non-matching lines.
    let (_s, mut c) = ucmd();
    c.args(&["-c", "-v", "a"])
        .pipe_in("a\nb\na\nc\n")
        .succeeds()
        .stdout_only("2\n");

    // Count per file with multiple files.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("f1", "a\nb\na\n");
    scene.fixtures.write("f2", "x\ny\n");
    c.args(&["-c", "a", "f1", "f2"])
        .succeeds()
        .stdout_only("f1:2\nf2:0\n");
}

#[test]
fn files_with_and_without_matches() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("hit", "yes\n");
    scene.fixtures.write("miss", "no\n");

    c.args(&["-l", "yes", "hit", "miss"])
        .succeeds()
        .stdout_only("hit\n");

    // -L: list files with no match. Exit code follows match semantics.
    // Since "missing" never matched anywhere, exit is 1.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("hit", "yes\n");
    scene.fixtures.write("miss", "no\n");
    c.args(&["-L", "missing", "hit", "miss"])
        .fails_with_code(1)
        .stdout_is("hit\nmiss\n");

    // -L with a pattern that DOES match in one file: the matching file is
    // excluded from the listing, so only the non-matching file is printed.
    // This exercises the early-return in `session_handle_match` taken when
    // `files_without_match` is set and a match is found (src/searcher.rs).
    let (scene, mut c) = ucmd();
    scene.fixtures.write("hit", "yes\n");
    scene.fixtures.write("miss", "no\n");
    c.args(&["-L", "yes", "hit", "miss"])
        .succeeds()
        .stdout_is("miss\n");

    // -l early-exits after the first match. Verify it doesn't print twice.
    // when the file has many.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("many", "x\nx\nx\nx\nx\n");
    c.args(&["-l", "x", "many"])
        .succeeds()
        .stdout_only("many\n");
}

#[test]
fn files_with_and_without_matches_mutually_exclusive() {
    // Test that -l and -L are mutually exclusive with last-one-wins semantics
    let (scene, mut c) = ucmd();
    scene.fixtures.write("file", "match\n");

    // -l -L: last flag (-L) wins, so no output (file has match, -L excludes it)
    c.args(&["-l", "-L", "match", "file"])
        .succeeds()
        .stdout_only("");

    // -L -l: last flag (-l) wins, so filename is printed
    let (scene, mut c) = ucmd();
    scene.fixtures.write("file", "match\n");
    c.args(&["-L", "-l", "match", "file"])
        .succeeds()
        .stdout_only("file\n");
}

#[test]
fn count_combined_with_listing_flags() {
    let (scene, _) = ucmd();
    scene.fixtures.write("hit", "yes\n");
    scene.fixtures.write("miss", "no\n");

    // -c + -l: -l wins, only filenames printed.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-c", "-l", "yes", "hit", "miss"])
        .succeeds()
        .stdout_only("hit\n");

    // -c + -o: -c wins (count, not matches).
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-c", "-o", "x"])
        .pipe_in("xxx\n")
        .succeeds()
        .stdout_only("1\n");
}

#[test]
fn only_matching() {
    // Multiple matches per line.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "-E", "foo"])
        .pipe_in("foo bar foo\nbaz\n")
        .succeeds()
        .stdout_only("foo\nfoo\n");

    // -o -n shows line number per match, not per line.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "-n", "-E", "x"])
        .pipe_in("xx\ny\nxx\n")
        .succeeds()
        .stdout_only("1:x\n1:x\n3:x\n3:x\n");

    // -o -b uses the byte offset of the match itself.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "-b", "-E", "ab"])
        .pipe_in("xxab yyab\n")
        .succeeds()
        .stdout_only("2:ab\n7:ab\n");

    // -o -i preserves the matched text's original case.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "-i", "hello"])
        .pipe_in("Hello\nhELLO\nHELLO!\n")
        .succeeds()
        .stdout_only("Hello\nhELLO\nHELLO\n");

    // Zero-width matches do not print in -o mode, but still mark the line as
    // matched. This matters for -v because matched lines must not be selected.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "$"]).pipe_in("\n").succeeds().no_output();

    let (_s, mut c) = ucmd();
    c.args(&["-o", "-v", "$"])
        .pipe_in("a\n\nb\n")
        .fails_with_code(1)
        .no_output();

    let (_s, mut c) = ucmd();
    c.args(&["-o", "-v", "x*"])
        .pipe_in("a\n\nb\n")
        .fails_with_code(1)
        .no_output();

    // After a match ends, ^ must not re-match at that position.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "^hello*"])
        .pipe_in("hellooo_hello\n")
        .succeeds()
        .stdout_only("hellooo\n");
}

#[test]
fn quiet_modes() {
    // Match: exit 0, no output. -q stops at the first match, so an EPIPE on the
    // harness's stdin write is expected.
    let (_s, mut c) = ucmd();
    c.args(&["-q", "a"])
        .ignore_stdin_write_error()
        .pipe_in("a\n")
        .succeeds()
        .no_output();

    // No match: exit 1, no output.
    let (_s, mut c) = ucmd();
    c.args(&["-q", "z"])
        .pipe_in("a\n")
        .fails_with_code(1)
        .no_output();

    // Quiet mode suppresses EOF bookkeeping output from -c and -L even when
    // the no-match path reaches finalization.
    let (_s, mut c) = ucmd();
    c.args(&["-q", "-c", "z"])
        .pipe_in("a\n")
        .fails_with_code(1)
        .no_output();

    let (_s, mut c) = ucmd();
    c.args(&["-q", "-L", "z"])
        .pipe_in("a\n")
        .fails_with_code(1)
        .no_output();
}

#[test]
fn auto_filename_prefix() {
    let (scene, _) = ucmd();
    scene.fixtures.write("a", "hit\n");
    scene.fixtures.write("b", "hit\n");

    // Single file: no prefix.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["hit", "a"]).succeeds().stdout_only("hit\n");

    // Multiple files: prefix shown.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["hit", "a", "b"])
        .succeeds()
        .stdout_only("a:hit\nb:hit\n");
}

#[test]
fn force_filename_flags() {
    let (scene, _) = ucmd();
    scene.fixtures.write("a", "hit\n");

    // -H forces prefix on a single file.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-H", "hit", "a"])
        .succeeds()
        .stdout_only("a:hit\n");

    // -h suppresses it even with multiple files.
    scene.fixtures.write("b", "hit\n");
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-h", "hit", "a", "b"])
        .succeeds()
        .stdout_only("hit\nhit\n");

    // Last-one-wins between -H and -h (clap overrides_with).
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-H", "-h", "hit", "a"])
        .succeeds()
        .stdout_only("hit\n");

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-h", "-H", "hit", "a"])
        .succeeds()
        .stdout_only("a:hit\n");
}

#[test]
fn label_only_applies_to_stdin() {
    // --label replaces "(standard input)".
    let (_s, mut c) = ucmd();
    c.args(&["--label=IN", "-H", "x"])
        .pipe_in("x\n")
        .succeeds()
        .stdout_only("IN:x\n");

    // For a real file, --label is ignored.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("real", "x\n");
    c.args(&["--label=IN", "-H", "x", "real"])
        .succeeds()
        .stdout_only("real:x\n");
}

#[test]
fn option_values_starting_with_hyphen() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("-list", "foo\n");
    c.args(&["-f", "-list"])
        .pipe_in("foo\nbar\n")
        .succeeds()
        .stdout_only("foo\n");

    let (_s, mut c) = ucmd();
    c.args(&["-H", "--label", "-in-", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_only("-in-:foo\n");

    let (_s, mut c) = ucmd();
    c.args(&["--group-separator", "-=-", "-A0", "x"])
        .pipe_in("x\ny\nx\n")
        .succeeds()
        .stdout_only("x\n-=-\nx\n");

    // --include, --exclude, --exclude-dir and --exclude-from also accept a
    // value that starts with a hyphen, in both `--opt=VALUE` and `--opt VALUE`
    // form.
    let (scene, _) = ucmd();
    scene.fixtures.mkdir_all("tree/-tmp");
    scene.fixtures.write("tree/keep.txt", "foo\n");
    scene.fixtures.write("tree/-odd.txt", "foo\n");
    scene.fixtures.write("tree/-tmp/hidden.txt", "foo\n");
    scene.fixtures.write("-excludes", "-odd.txt\n");

    let cases: &[(&[&str], &[&str], &[&str])] = &[
        (
            &["--include=-odd.txt"],
            &["-odd.txt"],
            &["keep.txt", "hidden.txt"],
        ),
        (
            &["--include", "-odd.txt"],
            &["-odd.txt"],
            &["keep.txt", "hidden.txt"],
        ),
        (
            &["--exclude=-odd.txt"],
            &["keep.txt", "hidden.txt"],
            &["-odd.txt"],
        ),
        (
            &["--exclude", "-odd.txt"],
            &["keep.txt", "hidden.txt"],
            &["-odd.txt"],
        ),
        (
            &["--exclude-dir=-tmp"],
            &["keep.txt", "-odd.txt"],
            &["hidden.txt"],
        ),
        (
            &["--exclude-dir", "-tmp"],
            &["keep.txt", "-odd.txt"],
            &["hidden.txt"],
        ),
        (
            &["--exclude-from=-excludes"],
            &["keep.txt", "hidden.txt"],
            &["-odd.txt"],
        ),
        (
            &["--exclude-from", "-excludes"],
            &["keep.txt", "hidden.txt"],
            &["-odd.txt"],
        ),
    ];
    for (opt, expected, unexpected) in cases {
        let mut args = vec!["-r", "-l"];
        args.extend_from_slice(opt);
        args.extend_from_slice(&["foo", "tree"]);
        let result = scene.cmd(env!("CARGO_BIN_EXE_grep")).args(&args).succeeds();
        for s in *expected {
            result.stdout_contains(*s);
        }
        for s in *unexpected {
            result.stdout_does_not_contain(*s);
        }
    }
}

#[test]
fn line_number_and_byte_offset_prefixes() {
    let (_s, mut c) = ucmd();
    c.args(&["-n", "b"])
        .pipe_in("a\nb\nc\nb\n")
        .succeeds()
        .stdout_only("2:b\n4:b\n");

    // Byte offset alone.
    let (_s, mut c) = ucmd();
    c.args(&["-b", "world"])
        .pipe_in("hello\nworld\n")
        .succeeds()
        .stdout_only("6:world\n");

    // Combined -n -b -H.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("f", "hello world\n");
    c.args(&["-H", "-n", "-b", "world", "f"])
        .succeeds()
        .stdout_only("f:1:0:hello world\n");

    // -T inserts a tab between the prefix and the content for alignment.
    // The amount of leading padding is implementation-defined (GNU pads
    // generously, this impl pads tightly), so we only assert the suffix.
    let (_s, mut c) = ucmd();
    c.args(&["-T", "-n", "x"])
        .pipe_in("x\n")
        .succeeds()
        .stdout_contains("1:\tx\n");

    // -T against a real file (not stdin): the line-number field width is
    // derived from the file size, which only happens on the `File` path in
    // `process_file` (src/searcher.rs), not the stdin path.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("f", "x\n");
    c.args(&["-T", "-n", "x", "f"])
        .succeeds()
        .stdout_contains("1:\tx\n");
}

#[test]
fn initial_tab_pads_to_the_input_size() {
    // GNU grep pads line numbers and byte offsets under -T to the number of
    // digits of the input's size; 100 bytes gives three columns.
    let (scene, mut c) = ucmd();
    let text = format!("{}x\n", "a".repeat(98));
    scene.fixtures.write("f", &text);
    let expected = format!("  1:  0:\t{text}");
    c.args(&["-T", "-n", "-b", "x", "f"])
        .succeeds()
        .stdout_only(&expected);

    // A regular file on stdin is sized the same way.
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-T", "-n", "-b", "x"])
        .set_stdin(std::fs::File::open(scene.fixtures.plus("f")).unwrap())
        .succeeds()
        .stdout_only(&expected);

    // A pipe has no size, so it gets the fixed width. On Windows the
    // metadata of a pipe looks like a regular file's.
    #[cfg(unix)]
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-bT", "x"])
        .pipe_in("x\n")
        .succeeds()
        .stdout_only(format!("{:>19}:\tx\n", 0));
}

#[test]
fn null_filename_separator() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("f", "x\n");
    c.args(&["-Z", "-l", "x", "f"])
        .succeeds()
        .stdout_only("f\0");
}

#[rstest]
#[case("-A", "1", "MATCH\ne\n")]
#[case("-B", "3", "b\nc\nd\nMATCH\n")]
#[case("-B", "2000000000", "a\nb\nc\nd\nMATCH\n")]
#[case("-C", "1", "d\nMATCH\ne\n")]
fn after_before_combined_context(
    #[case] option: &str,
    #[case] count: &str,
    #[case] expected: &str,
) {
    let (_s, mut c) = ucmd();
    c.args(&[option, count, "MATCH"])
        .pipe_in("a\nb\nc\nd\nMATCH\ne\nf\n")
        .succeeds()
        .stdout_only(expected);
}

#[test]
fn num_shorthand_is_context() {
    // `-2` is shorthand for `-C 2`.
    let (_s, mut c) = ucmd();
    c.args(&["-2", "MATCH"])
        .pipe_in("a\nb\nMATCH\nc\nd\n")
        .succeeds()
        .stdout_only("a\nb\nMATCH\nc\nd\n");
}

#[test]
fn num_shorthand_in_short_option_clusters() {
    // Five lines of leading and trailing context around the match.
    const CTX: &str = "zero\none\ntwo\nthree\nfour\nMATCH\nsix\nseven\neight\nnine\nten\n";
    let ok = |args: &[&str], expected: &str| {
        let (_s, mut c) = ucmd();
        c.args(args).pipe_in(CTX).succeeds().stdout_only(expected);
    };

    // A shorthand bundled behind another short option (the reported case).
    ok(&["-w2", "MATCH"], "three\nfour\nMATCH\nsix\nseven\n");
    ok(&["-i2", "match"], "three\nfour\nMATCH\nsix\nseven\n");

    // A run of digits is a single number, so `-w44` is 44 lines of context
    // rather than two separate `-4` options.
    ok(&["-w44", "MATCH"], CTX);

    // Runs split by another option stay separate values, and the last wins.
    ok(&["-w1x2", "MATCH"], "three\nfour\nMATCH\nsix\nseven\n");

    // Order is preserved, so a later option only overrides what it names.
    ok(&["-w2C1", "MATCH"], "four\nMATCH\nsix\n");
    ok(&["-2A1", "MATCH"], "three\nfour\nMATCH\nsix\n");

    // Regression for the original fuzzer case: trailing flags in the same
    // cluster are still parsed after the numeric options.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "a", "-w44ov"])
        .pipe_in("a\nb\n")
        .succeeds()
        .stdout_only("");
}

#[test]
fn num_shorthand_does_not_capture_option_values() {
    let ok = |args: &[&str], input: &str, expected: &str| {
        let (_s, mut c) = ucmd();
        c.args(args).pipe_in(input).succeeds().stdout_only(expected);
    };

    // Digits attached to a value-taking short option are its value.
    ok(&["-e123"], "123\n456\n", "123\n");
    ok(&["-m2", "x"], "x\nx\nx\n", "x\nx\n");
    ok(&["-wm2", "x"], "x\nx\nx\n", "x\nx\n");

    // Only the first value option in a cluster takes a value, so the trailing
    // `e` is `-e`'s value here and the following `-2` is still a shorthand.
    ok(&["-ee", "-2"], "a\nbee\nc\nd\n", "a\nbee\nc\nd\n");

    // A detached value is left alone, even after an expanded cluster.
    ok(&["-e", "-2"], "-2\nx\n", "-2\n");
    ok(&["--regexp", "-2"], "-2\nx\n", "-2\n");
    ok(&["-i2e", "-3"], "a\n-3\nb\n", "a\n-3\nb\n");

    // Non-option arguments are never shorthand.
    ok(&["--regexp=-2"], "-2\nx\n", "-2\n");
    ok(&["--", "-2"], "-2\nx\n", "-2\n");
    ok(&["w2"], "w2\nw3\n", "w2\n");
}

#[test]
fn abbreviated_long_options() {
    // (args, input, expected stdout)
    let cases: &[(&[&str], &str, &str)] = &[
        (&["--cou", "a"], "a\nb\na\n", "2\n"),
        (&["--reg=b"], "a\nb\n", "b\n"),
        (&["--max", "1", "a"], "a\na\n", "a\n"),
        (&["--ignore", "A"], "a\nb\n", "a\n"),
        (&["--colo=never", "a"], "a\nb\n", "a\n"),
        // An abbreviated value option still takes `-7` as its value.
        (&["--reg", "-7"], "-7\nx\n", "-7\n"),
        (&["--binary-f", "text", "a"], "a\nb\n", "a\n"),
        // An exact name wins over a longer option it is a prefix of, so
        // `--binary` is the flag and `-1` is the context shorthand.
        (&["--binary", "-1", "b"], "a\nb\nc\n", "a\nb\nc\n"),
    ];
    for (args, input, expected) in cases {
        let (_s, mut c) = ucmd();
        c.args(args)
            .pipe_in(*input)
            .succeeds()
            .stdout_only(*expected);
    }

    // An ambiguous prefix stays a usage error.
    for prefix in ["--co", "--fi"] {
        let (_s, mut c) = ucmd();
        c.args(&[prefix, "a"])
            .ignore_stdin_write_error()
            .pipe_in("a\n")
            .fails_with_code(2)
            .no_stdout();
    }
}

#[test]
fn num_shorthand_allows_non_ascii_attached_values() {
    // A non-ASCII attached value is legitimate: `-1e🙂` is `-C 1 -e 🙂`.
    let ok = |args: &[&str]| {
        let (_s, mut c) = ucmd();
        c.args(args)
            .pipe_in("a\n🙂\nb\nc\n")
            .succeeds()
            .stdout_only("a\n🙂\nb\n");
    };
    ok(&["-1e🙂"]);
    ok(&["-w1e🙂"]);

    // A digit only reachable past a non-ASCII byte is not a shorthand, leaving
    // the cluster the invalid option it would be without the digit.
    let fails = |args: &[&str]| {
        let (_s, mut c) = ucmd();
        // A usage error exits before reading stdin, so the write may hit EPIPE.
        c.args(args)
            .ignore_stdin_write_error()
            .pipe_in("x\n")
            .fails()
            .code_is(2);
    };
    fails(&["-🙂2", "x"]);
    fails(&["-2🙂3", "x"]);
}

#[cfg(unix)]
#[test]
fn num_shorthand_preserves_invalid_utf8_arguments() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    // Arguments that are not valid UTF-8 reach clap without being sliced apart.
    let (_s, mut c) = ucmd();
    c.arg(OsString::from_vec(vec![b'-', b'w', b'2', 0xff]))
        .arg("x")
        .ignore_stdin_write_error()
        .pipe_in("x\n")
        .fails();
}
#[test]
fn context_line_prefixes_use_dash() {
    // Match line uses `:`, context lines use `-`.
    let (_s, mut c) = ucmd();
    c.args(&["-n", "-A", "1", "MATCH"])
        .pipe_in("a\nMATCH\nb\n")
        .succeeds()
        .stdout_only("2:MATCH\n3-b\n");
}

#[test]
fn group_separator_behavior() {
    let input = "M\nx\nx\nx\nM\n";

    // Default `--` between non-adjacent match groups.
    let (_s, mut c) = ucmd();
    c.args(&["-C", "0", "M"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("M\n--\nM\n");

    // --no-group-separator suppresses it.
    let (_s, mut c) = ucmd();
    c.args(&["--no-group-separator", "-C", "0", "M"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("M\nM\n");

    // Custom separator.
    let (_s, mut c) = ucmd();
    c.args(&["--group-separator=***", "-C", "0", "M"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("M\n***\nM\n");
}

#[test]
fn adjacent_context_groups_do_not_get_separator() {
    let (_s, mut c) = ucmd();
    c.args(&["-e", ".", "-B", "2"])
        .pipe_in("a\nb\n\n\nc\n")
        .succeeds()
        .stdout_only("a\nb\n\n\nc\n");
}

#[test]
fn overlapping_context_not_duplicated() {
    let (_s, mut c) = ucmd();
    c.args(&["-n", "-C", "1", "-E", "b|d"])
        .pipe_in("a\nb\nc\nd\ne\n")
        .succeeds()
        .stdout_only("1-a\n2:b\n3-c\n4:d\n5-e\n");
}

#[test]
fn color_always_emits_sgr_el_sequence() {
    let (_s, mut c) = ucmd();
    c.args(&["--color=always", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[01;31m\x1b[Kfoo\x1b[m\x1b[K");
}

#[test]
fn color_never_emits_no_escapes() {
    let (_s, mut c) = ucmd();
    c.args(&["--color=never", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_only("foo\n");
}

// Guard against treating empty TERM or "DUMB" as "dumb".
#[cfg(unix)]
#[rstest]
#[case::unset(None, false)]
#[case::dumb(Some("dumb"), false)]
#[case::empty(Some(""), true)]
#[case::dumb_uppercase(Some("DUMB"), true)]
#[case::xterm(Some("xterm"), true)]
fn color_on_a_terminal_needs_option_and_term_not_dumb(
    #[case] term: Option<&str>,
    #[case] term_has_color: bool,
    #[values(None, Some("--color"), Some("--color=auto"))] option: Option<&str>,
) {
    let (_s, mut c) = ucmd();
    if let Some(term) = term {
        c.env("TERM", term);
    }
    if let Some(option) = option {
        c.arg(option);
    }
    let expected = if term_has_color && option.is_some() {
        "\x1b[01;31m\x1b[Kfoo\x1b[m\x1b[K\n"
    } else {
        "foo\n"
    };
    c.terminal_sim_stdio(TerminalSimulation {
        stdout: true,
        ..Default::default()
    })
    .arg("foo")
    .pipe_in("foo\n")
    .succeeds()
    .no_stderr()
    .normalized_newlines_stdout_is(expected);
}

#[test]
fn grep_colors_mt_sets_match_color() {
    // `mt` in GREP_COLORS sets the matched-text color (equivalent to ms + mc).
    let (_s, mut c) = ucmd();
    c.env("GREP_COLORS", "mt=36")
        .args(&["--color=always", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[36m\x1b[Kfoo\x1b[m\x1b[K");
}

#[test]
fn grep_color_env_is_deprecated_with_warning() {
    // GREP_COLOR still selects the match color but, when color is active, emits
    // GNU's deprecation warning pointing at GREP_COLORS 'mt'.
    let (_s, mut c) = ucmd();
    c.env("GREP_COLOR", "36")
        .args(&["--color=always", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[36m\x1b[Kfoo\x1b[m\x1b[K")
        .stderr_contains("warning: GREP_COLOR='36' is deprecated; use GREP_COLORS='mt=36'");
}

#[test]
fn grep_color_warning_only_when_used() {
    // GREP_COLORS sets both match colors, so GREP_COLOR is not used.
    for grep_colors in ["mt=35", "ms=35:mc=34"] {
        let (_s, mut c) = ucmd();
        c.env("GREP_COLOR", "36")
            .env("GREP_COLORS", grep_colors)
            .args(&["--color=always", "foo"])
            .pipe_in("foo\n")
            .succeeds()
            .no_stderr();
    }

    // No search happens, so no color is used either.
    for args in [["-m0", "foo"], ["-v", ""]] {
        let (_s, mut c) = ucmd();
        c.env("GREP_COLOR", "36")
            .arg("--color=always")
            .args(&args)
            .pipe_in("foo\n")
            // With -m0, grep exits before reading stdin.
            .ignore_stdin_write_error()
            .fails_with_code(1)
            .no_output();
    }

    // Only the selected color is overridden: the context color still comes from GREP_COLOR.
    let (_s, mut c) = ucmd();
    c.env("GREP_COLOR", "36")
        .env("GREP_COLORS", "ms=35")
        .args(&["--color=always", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stderr_contains("warning: GREP_COLOR='36' is deprecated; use GREP_COLORS='mt=36'");
}

#[test]
fn grep_color_env_no_warning_without_color() {
    // Without active color output, GREP_COLOR must not trigger the warning.
    let (_s, mut c) = ucmd();
    c.env("GREP_COLOR", "36")
        .args(&["--color=never", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_only("foo\n");
}

#[test]
fn grep_color_env_malformed_value_is_ignored() {
    let (_s, mut c) = ucmd();
    c.env("GREP_COLOR", "4x2")
        .args(&["--color=always", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_only("\x1b[01;31m\x1b[Kfoo\x1b[m\x1b[K\n");
}

#[test]
fn color_line_number_uses_green() {
    let (_s, mut c) = ucmd();
    c.args(&["--color=always", "-n", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[32m\x1b[K1\x1b[m\x1b[K");
}

#[test]
fn color_count_colors_file_name_and_separator() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("a", "foo\nfoo\n");
    scene.fixtures.write("b", "bar\n");
    c.args(&["--color=always", "-c", "foo", "a", "b"])
        .succeeds()
        .stdout_only(
            "\x1b[35m\x1b[Ka\x1b[m\x1b[K\x1b[36m\x1b[K:\x1b[m\x1b[K2\n\
             \x1b[35m\x1b[Kb\x1b[m\x1b[K\x1b[36m\x1b[K:\x1b[m\x1b[K0\n",
        );

    let (scene, mut c) = ucmd();
    scene.fixtures.write("a", "foo\nfoo\n");
    c.args(&["--color=always", "-c", "--null", "-H", "foo", "a"])
        .succeeds()
        .stdout_only("\x1b[35m\x1b[Ka\x1b[m\x1b[K\x002\n");
}

#[test]
fn color_with_ignore_case_preserves_original_text() {
    let (_s, mut c) = ucmd();
    c.args(&["--color=always", "-i", "word"])
        .pipe_in("Word\nwORD\n")
        .succeeds()
        .stdout_contains("Word")
        .stdout_contains("wORD")
        .stdout_contains("\x1b[01;31m\x1b[KWord\x1b[m\x1b[K")
        .stdout_contains("\x1b[01;31m\x1b[KwORD\x1b[m\x1b[K");
}

#[test]
fn color_anchored_pattern_no_rematch_at_match_end() {
    let (_s, mut c) = ucmd();
    c.args(&["--color=always", "^word_*"])
        .pipe_in("word_word\n")
        .succeeds()
        .stdout_contains("\x1b[01;31m\x1b[Kword_\x1b[m\x1b[K");
}

#[test]
fn grep_colors_env_overrides() {
    let (_s, mut c) = ucmd();
    c.env("GREP_COLORS", "ms=33:ln=34")
        .args(&["--color=always", "-n", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[33m\x1b[Kfoo\x1b[m\x1b[K")
        .stdout_contains("\x1b[34m\x1b[K1\x1b[m\x1b[K");
}

#[test]
fn grep_colors_env_stops_at_malformed_value() {
    let (_s, mut c) = ucmd();
    c.env("GREP_COLORS", "fn=4x2:ms=33")
        .args(&["--color=always", "-H", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[35m\x1b[K(standard input)\x1b[m\x1b[K")
        .stdout_contains("\x1b[01;31m\x1b[Kfoo\x1b[m\x1b[K");
}

#[test]
fn legacy_grep_color_env() {
    // uutests clears the environment by default,
    // so we need to set GREP_COLORS explicitly.
    let (_s, mut c) = ucmd();
    c.env("GREP_COLOR", "44")
        .args(&["--color=always", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[44m\x1b[Kfoo\x1b[m\x1b[K");
}

#[test]
fn binary_detection_via_nul_byte() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write_bytes("b", b"hit\0\n");
    c.args(&["hit", "b"])
        .succeeds()
        .no_stdout()
        .stderr_contains("binary file matches");
}

#[test]
fn binary_detection_via_invalid_utf8() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write_bytes("b", b"a\x9db\n");
    c.args(&["a", "b"])
        .succeeds()
        .no_stdout()
        .stderr_contains("binary file matches");
}

#[test]
fn lone_control_byte_is_still_text() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write_bytes("ctl", b"a\x01b\n");
    c.args(&["a", "ctl"])
        .succeeds()
        .stdout_is_bytes(b"a\x01b\n")
        .no_stderr();
}

// Whether the bad byte appears before or after the match, the matching
// line must still be returned and the file must not be reported as binary.
#[rstest]
#[case::before(b"x\x9dy\nhit\n")]
#[case::after(b"hit\nx\x9dy\n")]
fn invalid_utf8_on_nonmatching_line_does_not_poison_file(#[case] content: &[u8]) {
    let (scene, mut c) = ucmd();
    scene.fixtures.write_bytes("in", content);
    c.args(&["hit", "in"])
        .succeeds()
        .stdout_is("hit\n")
        .no_stderr();
}

#[test]
fn binary_files_text_forces_text_mode() {
    let (scene, _) = ucmd();
    scene.fixtures.write_bytes("b", b"hit\0more\n");

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-a", "hit", "b"])
        .succeeds()
        .stdout_contains("hit");

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["--binary-files=text", "hit", "b"])
        .succeeds()
        .stdout_contains("hit");
}

#[test]
fn binary_files_without_match_skips() {
    let (scene, _) = ucmd();
    scene.fixtures.write_bytes("b", b"hit\0more\n");
    scene.fixtures.write_bytes("invalid", b"a\x9db\n");

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-I", "hit", "b"]).fails_with_code(1).no_output();

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["--binary-files=without-match", "hit", "b"])
        .fails_with_code(1)
        .no_output();

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-I", "a", "invalid"])
        .succeeds()
        .stdout_is_bytes(b"a\x9db\n")
        .no_stderr();

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["--binary-files=without-match", "a", "invalid"])
        .succeeds()
        .stdout_is_bytes(b"a\x9db\n")
        .no_stderr();
}

fn build_tree(scene: &TestScenario) {
    scene.fixtures.mkdir_all("tree");
    scene.fixtures.mkdir_all("tree/sub");
    scene.fixtures.write("tree/a.txt", "grep me\n");
    scene.fixtures.write("tree/b.log", "grep me\n");
    scene.fixtures.write("tree/sub/c.txt", "grep me\n");
}

#[test]
fn recursive_default() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-r", "-l", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_contains("b.log")
        .stdout_contains("c.txt");
}

#[test]
fn recursive_no_file_defaults_to_cwd_not_stdin() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.current_dir(scene.fixtures.plus("tree"))
        .args(&["-r", "-l", "grep"])
        // We did NOT pipe in anything; if the impl tried to read stdin we'd
        // hang or fail. cwd should be searched instead.
        .succeeds()
        .stdout_contains("a.txt");
}

// GNU grep reduces the trailing slashes of a directory operand to one,
// but keeps the slashes inside it.
#[rstest]
#[case("", "tree//sub//", "tree//sub/c.txt:grep me\n")]
#[case("", "tree/sub///", "tree/sub/c.txt:grep me\n")]
#[case("", "tree/sub/", "tree/sub/c.txt:grep me\n")]
#[case("tree/sub", ".//", "./c.txt:grep me\n")]
fn recursive_collapses_repeated_trailing_slashes(
    #[case] dir: &str,
    #[case] operand: &str,
    #[case] expected: &str,
) {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.current_dir(scene.fixtures.plus(dir))
        .args(&["-r", "grep", operand])
        .succeeds()
        .stdout_only(expected);
}

#[test]
fn recursive_implicit_cwd_strips_dot_prefix() {
    // `-r` with no path argument searches the implicit ".", so reported paths
    // come back as "./only.txt" (or ".\\only.txt" on Windows). GNU strips that
    // leading prefix; `strip_dot_prefix` in src/searcher.rs must do the same.
    // A single file keeps the output deterministic and lets us assert the exact
    // line, which `stdout_contains` in `recursive_no_file_defaults_to_cwd_not_stdin`
    // cannot (it would also pass with a leaked "./" prefix).
    let (scene, _) = ucmd();
    scene.fixtures.mkdir_all("flat");
    scene.fixtures.write("flat/only.txt", "grep me\n");

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.current_dir(scene.fixtures.plus("flat"))
        .args(&["-r", "grep"])
        .succeeds()
        .stdout_is("only.txt:grep me\n");
}

// exclude-dir filters whole directories; like GNU grep, trailing
// slashes in the name are ignored.
#[rstest]
fn recursive_exclude_dir_ignores_trailing_slashes(
    #[values("--exclude-dir=sub", "--exclude-dir=sub/", "--exclude-dir=sub//")] value: &str,
) {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-r", "-l", value, "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("c.txt");
}

#[test]
fn recursive_with_include_exclude() {
    let (scene, _) = ucmd();
    build_tree(&scene);

    // include filters in.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-r", "-l", "--include=*.txt", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("b.log");

    // exclude filters out.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-r", "-l", "--exclude=*.log", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("b.log");

    // include + exclude both apply (exclude wins on conflict).
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&[
        "-r",
        "-l",
        "--include=*.txt",
        "--include=*.log",
        "--exclude=*.log",
        "grep",
        "tree",
    ])
    .succeeds()
    .stdout_contains("a.txt")
    .stdout_does_not_contain("b.log");
}

// a.txt matches both, and the later --include wins. b.log matches
// neither and is kept, since the first pattern is an --exclude.
#[rstest]
fn recursive_include_exclude_last_match_wins(
    #[values("--exclude=a*", "--exclude=zz")] first: &str,
) {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-r", "-l", first, "--include=*.txt", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_contains("b.log")
        .stdout_contains("c.txt");
}

#[test]
fn recursive_exclude_from_counts_at_its_position() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    scene.fixtures.write("excludes", "*.log\n");

    // --exclude-from comes last here, so b.log is excluded, and the .txt
    // files match nothing after a first --include.
    c.args(&[
        "-r",
        "-l",
        "--include=*.log",
        "--exclude-from=excludes",
        "grep",
        "tree",
    ])
    .fails_with_code(1)
    .no_stdout();
}

#[test]
fn recursive_exclude_from_file() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    scene.fixtures.write("excludes", "*.log\n");
    c.args(&["-r", "-l", "--exclude-from=excludes", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("b.log");
}

#[test]
fn exclude_from_dash_reads_standard_input() {
    // GNU grep reads the exclude list from standard input when the operand is
    // `-`; uu_grep used to look for a file literally named `-`.
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-r", "-l", "--exclude-from=-", "grep", "tree"])
        .pipe_in("*.log\n")
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("b.log");
}

#[cfg(all(unix, not(target_os = "cygwin")))]
#[test]
fn dereference_recursive_follows_symlinks() {
    use std::os::unix::fs::symlink;

    let (scene, _) = ucmd();
    scene.fixtures.mkdir_all("tree");
    scene.fixtures.mkdir_all("target");
    scene.fixtures.write("target/hit.txt", "grep me\n");
    symlink(
        scene.fixtures.plus("target"),
        scene.fixtures.plus("tree/link"),
    )
    .unwrap();

    // -R must follow the symlink.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-R", "-l", "grep", "tree"])
        .succeeds()
        .stdout_contains("hit.txt");

    // -r must NOT follow it.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-r", "-l", "grep", "tree"])
        .fails_with_code(1)
        .no_output();
}

#[test]
fn directories_skip_silently() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-d", "skip", "grep", "tree"])
        .fails_with_code(1)
        .no_output();
}

#[test]
fn directories_read_errors() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-d", "read", "grep", "tree"])
        .fails_with_code(2)
        .stderr_contains("tree");
}

// A directory opens but fails to read. GNU still prints its count for `-c`
// and its name for `-L`. (On Windows a directory does not open at all.)
// `foo` takes the literal fast path, `fo*` the line-at-a-time one; `-v` never
// takes the fast path.
#[cfg(unix)]
#[rstest]
#[case::count_fast("-c", "foo", "dir:0\na.txt:1\n")]
#[case::count_regex("-c", "fo*", "dir:0\na.txt:1\n")]
#[case::count_invert("-cv", "foo", "dir:0\na.txt:0\n")]
#[case::without_match_fast("-L", "foo", "dir\n")]
#[case::without_match_regex("-L", "fo*", "dir\n")]
#[case::with_matches_fast("-l", "foo", "a.txt\n")]
#[case::with_matches_regex("-l", "fo*", "a.txt\n")]
fn count_and_files_without_match_list_unreadable_input(
    #[case] option: &str,
    #[case] pattern: &str,
    #[case] stdout: &str,
) {
    let (scene, mut c) = ucmd();
    scene.fixtures.mkdir("dir");
    scene.fixtures.write("a.txt", "foo\n");
    c.args(&[option, pattern, "dir", "a.txt"])
        .fails_with_code(2)
        .stdout_is(stdout)
        .stderr_is("grep: dir: Is a directory\n");
}

#[test]
fn directories_recurse_equivalent_to_dash_r() {
    let (scene, mut c) = ucmd();
    build_tree(&scene);
    c.args(&["-d", "recurse", "-l", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt");
}

#[cfg(unix)]
#[test]
fn recursive_skips_fifos_by_default() {
    use std::process::Command;

    let (scene, _) = ucmd();
    build_tree(&scene);

    // Reading this FIFO would block forever, so the test
    // implicitly proves it was skipped if grep returns.
    let fifo_path = scene.fixtures.plus("tree/fifo");
    let status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("mkfifo failed");
    assert!(status.success(), "could not create FIFO");

    // Default (no -D): FIFO is skipped, regular file matches.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-r", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("fifo");

    // Explicit -D skip: same behavior.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-r", "-D", "skip", "grep", "tree"])
        .succeeds()
        .stdout_contains("a.txt")
        .stdout_does_not_contain("fifo");
}

#[cfg(unix)]
#[test]
fn device_skip_on_explicit_special_file_arg() {
    use std::process::Command;

    // A special file (FIFO) named *directly* as an argument, not via recursion.
    // With `-D skip` it must be dropped without reading (reading would block
    // forever, so the test returning at all proves it was skipped). This covers
    // the top-level special-file branch in `process_path` and `is_special_file`
    // (src/searcher.rs), distinct from the recursive FIFO path.
    let (scene, _) = ucmd();
    let fifo_path = scene.fixtures.plus("fifo");
    let status = Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("mkfifo failed");
    assert!(status.success(), "could not create FIFO");

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-D", "skip", "grep", "fifo"])
        .fails_with_code(1)
        .no_output();
}

#[test]
fn nonexistent_file_is_error() {
    let (_s, mut c) = ucmd();
    c.args(&["x", "does-not-exist"])
        .fails_with_code(2)
        .stderr_contains("does-not-exist");
}

#[test]
fn nonexistent_file_error_has_no_os_error_suffix() {
    // GNU prints "grep: <file>: No such file or directory" with no
    // " (os error 2)" suffix; strip_errno keeps us byte-compatible. The
    // underlying OS message text differs on Windows, but in both cases the
    // trailing " (os error N)" must be absent.
    #[cfg(not(windows))]
    let expected = "grep: does-not-exist: No such file or directory\n";
    #[cfg(windows)]
    let expected = "grep: does-not-exist: The system cannot find the file specified.\n";

    let (_s, mut c) = ucmd();
    c.args(&["x", "does-not-exist"])
        .fails_with_code(2)
        .stderr_is(expected);
}

#[test]
fn dash_argument_means_stdin() {
    let (_s, mut c) = ucmd();
    c.args(&["x", "-"])
        .pipe_in("x\ny\n")
        .succeeds()
        .stdout_only("x\n");
}

#[test]
fn empty_file_exits_one() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    c.args(&["x", "empty"]).fails_with_code(1).no_output();
}

#[test]
fn empty_stdin_exits_one() {
    let (_s, mut c) = ucmd();
    c.args(&["x"]).pipe_in("").fails_with_code(1).no_output();
}

#[test]
fn missing_trailing_newline_still_matched() {
    let (_s, mut c) = ucmd();
    c.args(&["x"]).pipe_in("x").succeeds().stdout_only("x\n");
}

#[test]
fn crlf_line_endings_are_stripped_on_output() {
    let (_s, mut c) = ucmd();
    c.args(&["x"])
        .pipe_in("x\r\ny\r\n")
        .succeeds()
        .stdout_only(if cfg!(windows) { "x\n" } else { "x\r\n" });
}

#[test]
fn empty_line_matches_anchored_empty_pattern() {
    let (_s, mut c) = ucmd();
    c.args(&["^$"])
        .pipe_in("a\n\nb\n")
        .succeeds()
        .stdout_only("\n");
}

#[test]
fn unicode_literal_matches() {
    let (_s, mut c) = ucmd();
    c.args(&["café"])
        .pipe_in("café au lait\ntea\n")
        .succeeds()
        .stdout_only("café au lait\n");
}

#[test]
fn null_data_mode_records() {
    // Records are delimited by NUL on input and output alike.
    let (_s, mut c) = ucmd();
    c.args(&["-z", "hello"])
        .pipe_in(&b"hello\0world\0"[..])
        .succeeds()
        .stdout_is_bytes(b"hello\0");

    // With NUL-delimited records, newline is ordinary data and `.` matches it.
    let (_s, mut c) = ucmd();
    c.args(&["-z", "-o", "."])
        .pipe_in(&b"a\nb"[..])
        .succeeds()
        .stdout_is_bytes(b"a\0\n\0b\0");

    // GNU grep's PCRE path currently does not let `.*` consume the extra
    // newline here under -z; this mirrors the GNU pcre-context test.
    let (_s, mut c) = ucmd();
    c.args(&["-P", "-z", "-o", r"(?<=\n\n\n).*"])
        .pipe_in(
            &b"NUL preceded by 0 empty lines.\0\
              \nNUL preceded by 1 empty line.\0\
              \n\nNUL preceded by 2 empty lines.\0\
              \n\n\nNUL preceded by 3 empty lines.\0\
              \n\n\n\nNUL preceded by 4 empty lines.\0\n"[..],
        )
        .succeeds()
        .stdout_is_bytes(b"NUL preceded by 3 empty lines.\0NUL preceded by 4 empty lines.\0");

    // Counting works under -z.
    let (_s, mut c) = ucmd();
    c.args(&["-z", "-c", "hello"])
        .pipe_in(&b"hello\0world\0"[..])
        .succeeds()
        .stdout_only("1\n");
}

#[rstest]
fn null_data_negated_bracket_matches_newline(#[values("-G", "-E")] mode: &str) {
    let (_s, mut c) = ucmd();
    c.args(&[mode, "-z", "-c", "k[^m]w"])
        .pipe_in("hawk\nwren\0")
        .succeeds()
        .stdout_only("1\n");
}

#[test]
fn exit_codes_basic_triad() {
    // 0: any match.
    let (_s, mut c) = ucmd();
    c.args(&["x"]).pipe_in("x\n").succeeds();

    // 1: no match.
    let (_s, mut c) = ucmd();
    c.args(&["x"]).pipe_in("y\n").fails_with_code(1);

    // 2: error (missing file).
    let (_s, mut c) = ucmd();
    c.args(&["x", "missing"]).fails_with_code(2);
}

#[test]
fn error_outranks_match_in_exit_code() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("real", "x\n");
    // -s suppresses the message but not the exit code.
    c.args(&["-s", "x", "real", "missing"])
        .fails_with_code(2)
        .stdout_contains("x")
        .no_stderr();
}

#[test]
fn help_and_version() {
    let (_s, mut c) = ucmd();
    c.args(&["--help"]).succeeds();

    let (_s, mut c) = ucmd();
    c.args(&["--version"])
        .succeeds()
        .stdout_contains(env!("CARGO_PKG_VERSION"));
}

#[test]
fn repeated_options_are_accepted() {
    // GNU grep tolerates options given more than once: boolean flags are
    // idempotent and value options take the last occurrence. clap would
    // otherwise error with "cannot be used multiple times".

    // Repeated boolean flags are a no-op (not an error).
    let (_s, mut c) = ucmd();
    c.args(&["-n", "-n", "a"])
        .pipe_in("abc\n")
        .succeeds()
        .stdout_only("1:abc\n");

    // Mixed repeated booleans behave like a single occurrence.
    let (_s, mut c) = ucmd();
    c.args(&["-i", "-i", "abc"])
        .pipe_in("ABC\n")
        .succeeds()
        .stdout_only("ABC\n");

    // Repeated value options take the last value (here: -m 1 wins). -m stops
    // grep before stdin is drained, hence the ignored write error.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "5", "-m", "1", "x"])
        .ignore_stdin_write_error()
        .pipe_in("x\nx\nx\n")
        .succeeds()
        .stdout_only("x\n");

    // -e (ArgAction::Append) must still accumulate every pattern.
    let (_s, mut c) = ucmd();
    c.args(&["-e", "a", "-e", "b"])
        .pipe_in("a\nb\nc\n")
        .succeeds()
        .stdout_only("a\nb\n");
}

#[test]
fn literal_buffer_path_prefixes_and_max() {
    // Plain literals are served by the buffer-at-a-time engine; the line/byte
    // prefixes and -m must still be byte-identical to the line-at-a-time path.

    // -n and -b together: "lineno:byteoffset:line".
    let (_s, mut c) = ucmd();
    c.args(&["-nb", "foo"])
        .pipe_in("foo\nbar\nfoobar\n")
        .succeeds()
        .stdout_only("1:0:foo\n3:8:foobar\n");

    // A line matched more than once is still emitted once.
    let (_s, mut c) = ucmd();
    c.args(&["-c", "oo"])
        .pipe_in("oooo\nbar\noo\n")
        .succeeds()
        .stdout_only("2\n");

    // -m caps printed matches; grep exits before stdin is drained.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "2", "x"])
        .ignore_stdin_write_error()
        .pipe_in("x\ny\nx\nz\nx\n")
        .succeeds()
        .stdout_only("x\nx\n");

    // Final line without a trailing terminator still matches and is printed
    // with an added newline.
    let (_s, mut c) = ucmd();
    c.args(&["foo"])
        .pipe_in("bar\nfoo")
        .succeeds()
        .stdout_only("foo\n");
}

#[test]
fn literal_buffer_path_spans_many_chunks() {
    // Build an input far larger than the read buffer so the buffer-at-a-time
    // engine crosses several chunk boundaries, and check that line numbers and
    // counts stay correct across them.
    let mut input = String::new();
    let mut expected_n = String::new();
    let mut count = 0u32;
    for i in 1..=100_000u32 {
        if i % 7 == 0 {
            input.push_str("needle\n");
            expected_n.push_str(&format!("{i}:needle\n"));
            count += 1;
        } else {
            input.push_str("some filler text\n");
        }
    }
    assert!(input.len() > 512 * 1024, "input must exceed several chunks");

    let (_s, mut c) = ucmd();
    c.args(&["-c", "needle"])
        .pipe_in(input.clone())
        .succeeds()
        .stdout_only(format!("{count}\n"));

    let (_s, mut c) = ucmd();
    c.args(&["-n", "needle"])
        .pipe_in(input)
        .succeeds()
        .stdout_only(expected_n);
}

// Plain literals run on the buffer-at-a-time fast path, so the following tests
// use bracket-class patterns (non-literal) to keep the line-at-a-time engine's
// `-l` / `-L` / `-q` and binary-handling paths exercised too.

#[test]
fn slow_path_list_and_quiet_modes() {
    let (scene, _) = ucmd();
    scene.fixtures.write("hit", "yes\n");
    scene.fixtures.write("miss", "no\n");

    // -l: list matching files.
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-l", "[y]es", "hit", "miss"])
        .succeeds()
        .stdout_is("hit\n");

    // -L with a match in one file: only the non-matching file is listed.
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-L", "[y]es", "hit", "miss"])
        .succeeds()
        .stdout_is("miss\n");

    // -L with no match anywhere: both files listed, exit 1.
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-L", "[z]z", "hit", "miss"])
        .fails_with_code(1)
        .stdout_is("hit\nmiss\n");

    // -q stops at the first match (exit 0) or reports no match (exit 1).
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-q", "[y]es", "hit"])
        .succeeds()
        .no_output();
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-q", "[z]z", "hit"])
        .fails_with_code(1)
        .no_output();
}

#[test]
fn slow_path_binary_handling() {
    let (scene, _) = ucmd();
    // NOTE: avoid the name "nul" here — it's a reserved device name on Windows,
    // so writing/reading it hits the null device instead of a real file.
    scene.fixtures.write_bytes("nulbin", b"hit\0\n");
    scene.fixtures.write_bytes("bad", b"a\x9d\n");

    // Binary notice on the line-at-a-time engine (regex pattern).
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["[h]it", "nulbin"])
        .succeeds()
        .no_stdout()
        .stderr_contains("binary file matches");

    // -a forces text mode: the NUL line is printed verbatim.
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["-a", "[h]it", "nulbin"])
        .succeeds()
        .stdout_is_bytes(b"hit\0\n");

    // A NUL after the matched line means binariness is discovered at EOF, so
    // the line is printed first and the notice is emitted during finalization.
    scene.fixtures.write_bytes("late", b"hit\nno\0\n");
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["[h]it", "late"])
        .succeeds()
        .stdout_is("hit\n")
        .stderr_contains("binary file matches");
}

#[test]
fn fast_path_binary_detected_after_a_printed_line() {
    // A NUL that appears only after the last match in the buffer marks the file
    // binary on the fast path *after* an earlier match was already printed: the
    // printed line stays and the trailing notice is still emitted.
    let (scene, _) = ucmd();
    scene.fixtures.write_bytes("b", b"hit\nno\0\n");
    scene
        .cmd(env!("CARGO_BIN_EXE_grep"))
        .args(&["hit", "b"])
        .succeeds()
        .stdout_is("hit\n")
        .stderr_contains("binary file matches");
}

#[test]
fn only_matching_with_context() {
    // `only_matching` should override any context arguments.
    let input = "aa\nbb\ncc\nxx\n";

    let (_s, mut c) = ucmd();
    c.args(&["aa", "-o", "-A", "2"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("aa\n");

    let (_s, mut c) = ucmd();
    c.args(&["xx", "-o", "-B", "2"])
        .pipe_in("aa\nbb\ncc\nxx\n")
        .succeeds()
        .stdout_only("xx\n");

    let (_s, mut c) = ucmd();
    c.args(&["cc", "-o", "-C", "2"])
        .pipe_in("aa\nbb\ncc\nxx\n")
        .succeeds()
        .stdout_only("cc\n");

    // With line numbers.
    let (_s, mut c) = ucmd();
    c.args(&["xx", "-o", "-C", "2", "-n"])
        .pipe_in("aa\nbb\ncc\nxx\n")
        .succeeds()
        .stdout_only("4:xx\n");
}

// GNU grep accepts these spellings of WHEN, compared case-insensitively.
#[rstest]
fn color_when_always_synonyms(#[values("yes", "force", "ALWAYS", "Always")] when: &str) {
    let (_s, mut c) = ucmd();
    c.arg(format!("--color={when}"))
        .arg("foo")
        .pipe_in("foo\n")
        .succeeds()
        .stdout_only("\x1b[01;31m\x1b[Kfoo\x1b[m\x1b[K\n");
}

#[rstest]
fn color_when_never_synonyms(#[values("no", "none", "NEVER", "tty", "if-tty", "Auto")] when: &str) {
    let (_s, mut c) = ucmd();
    c.arg(format!("--colour={when}"))
        .arg("foo")
        .pipe_in("foo\n")
        .succeeds()
        .stdout_only("foo\n");
}

#[rstest]
fn ere_empty_interval_is_error(
    #[values("zq{}", "(zq){}", "[z]{}", "z*{}", r"\({}", r"a\.{}")] pattern: &str,
) {
    let (_s, mut c) = ucmd();
    c.args(&["-E", pattern])
        .pipe_in("zq{}\n")
        .ignore_stdin_write_error()
        .fails_with_code(2)
        .no_stdout()
        .stderr_contains("Invalid content of \\{\\}");
}

// A brace that cannot start an interval stays literal.
#[rstest]
fn ere_brace_without_interval_is_literal(
    #[values("zq{w", "{}", "x|{}", "({})", "zq\\{}")] pattern: &str,
) {
    let (_s, mut c) = ucmd();
    c.args(&["-E", "-c", pattern])
        .pipe_in("zq{w\nzq{}\n")
        .succeeds()
        .stdout_only("1\n");
}

#[test]
fn ere_trailing_backslash_is_error() {
    // A trailing backslash does not panic during interval inspection.
    let (_s, mut c) = ucmd();
    c.args(&["-E", "a\\"])
        .pipe_in("test\n")
        .ignore_stdin_write_error()
        .fails_with_code(2)
        .no_stdout();
}

#[cfg(unix)]
#[test]
fn non_utf8_pattern_file_name_and_label() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    // --label is purely a display prefix and works across all Unix platforms including macOS.
    let (_s, mut c) = ucmd();
    c.arg("-H")
        .arg(OsStr::from_bytes(b"--label=in\xfe"))
        .arg("foo")
        .pipe_in("foo\n")
        .succeeds()
        .stdout_is_bytes(b"in\xfe:foo\n");

    let (_s, mut c) = ucmd();
    c.arg("-H")
        .arg("-Z")
        .arg(OsStr::from_bytes(b"--label=in\xfe"))
        .arg("foo")
        .pipe_in("foo\n")
        .succeeds()
        .stdout_is_bytes(b"in\xfe\0foo\n");

    let (_s, mut c) = ucmd();
    c.arg("-l")
        .arg(OsStr::from_bytes(b"--label=in\xfe"))
        .arg("foo")
        .pipe_in("foo\n")
        .succeeds()
        .stdout_is_bytes(b"in\xfe\n");

    let (_s, mut c) = ucmd();
    c.arg("-l")
        .arg("-Z")
        .arg(OsStr::from_bytes(b"--label=in\xfe"))
        .arg("foo")
        .pipe_in("foo\n")
        .succeeds()
        .stdout_is_bytes(b"in\xfe\0");

    let (_s, mut c) = ucmd();
    c.arg("-L")
        .arg(OsStr::from_bytes(b"--label=in\xfe"))
        .arg("bar")
        .pipe_in("foo\n")
        .fails_with_code(1)
        .stdout_is_bytes(b"in\xfe\n");

    // Creating files with non-UTF-8 names is rejected by Darwin/macOS APFS with EILSEQ (errno 92).
    // On Linux and other Unix filesystems, arbitrary byte file names are fully supported.
    if !cfg!(target_os = "macos") {
        let name = OsStr::from_bytes(b"list\xfe");
        let (scene, mut c) = ucmd();
        std::fs::write(scene.fixtures.plus(name), "foo\n").unwrap();
        c.arg("-f")
            .arg(name)
            .pipe_in("foo\nbar\n")
            .succeeds()
            .stdout_only("foo\n");

        let excl_name = OsStr::from_bytes(b"excl\xfe");
        let (scene, mut c) = ucmd();
        std::fs::write(scene.fixtures.plus(excl_name), "*.txt\n").unwrap();
        scene.fixtures.write("match.txt", "foo\n");
        c.args(&["-r", "foo"])
            .arg(OsStr::from_bytes(b"--exclude-from=excl\xfe"))
            .fails_with_code(1)
            .no_stdout();
    }
}

#[test]
fn only_matching_empty_match_does_not_split_utf8() {
    let (_s, mut c) = ucmd();
    c.args(&["-ob", r"-e\w*"])
        .pipe_in("run \u{1F635}rm now\n")
        .succeeds()
        .stdout_only("0:run\n8:rm\n11:now\n");

    let (_s, mut c) = ucmd();
    c.args(&["-o", "-e[^、]*"])
        .pipe_in("a、b\n")
        .succeeds()
        .stdout_only("a\nb\n");

    // Invalid leading byte after an empty match: step falls back to 1,
    // so the next search starts at the following byte (offset 3).
    let (_s, mut c) = ucmd();
    c.args(&["-a", "-ob", "-ea*"])
        .pipe_in(&b"xa\xffa\n"[..])
        .succeeds()
        .stdout_only("1:a\n3:a\n");

    // --color must also skip the whole character, not highlight a torn tail.
    let red = "\x1b[01;31m\x1b[K";
    let reset = "\x1b[m\x1b[K";
    let (_s, mut c) = ucmd();
    c.args(&["--color=always", r"-e\w*"])
        .pipe_in("run \u{1F635}rm now\n")
        .succeeds()
        .stdout_only(format!(
            "{red}run{reset} \u{1F635}{red}rm{reset} {red}now{reset}\n"
        ));
}

// A pattern is matched against the operand as written and against paths
// relative to it; one with a slash must match that whole path.
#[rstest]
#[case("./box", "./box", true)]
#[case("./b*", "./box", true)]
#[case("box", "./box", true)]
#[case("./inner", "./box", false)]
#[case("box/inner", "./box", false)]
#[case("./box/inner", "./box", true)]
#[case("box/inner", "box", true)]
fn exclude_dir_matches_operand_as_written(
    #[case] pattern: &str,
    #[case] operand: &str,
    #[case] excluded: bool,
) {
    let (scene, mut c) = ucmd();
    scene.fixtures.mkdir_all("box/inner");
    scene.fixtures.write("box/inner/f.txt", "hay\n");
    let result = c
        .args(&["-r", &format!("--exclude-dir={pattern}"), "hay", operand])
        .run();
    if excluded {
        result.code_is(1).no_output();
    } else {
        result.success().stdout_contains("f.txt:hay").no_stderr();
    }
}

#[cfg(unix)]
#[test]
fn diagnostics_follow_earlier_output() {
    use std::process::{Command, Stdio};
    let (scene, _) = ucmd();
    scene.fixtures.write("a.txt", "foo a\n");
    scene.fixtures.write("bin.dat", "foo\0bin\n");
    scene.fixtures.write("b.txt", "foo b\n");
    let log = std::fs::File::create(scene.fixtures.plus("log")).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_grep"))
        .current_dir(scene.fixtures.as_string())
        .args(["foo", "a.txt", "bin.dat", "missing.txt", "b.txt"])
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert_eq!(
        scene.fixtures.read("log"),
        "a.txt:foo a\ngrep: bin.dat: binary file matches\ngrep: missing.txt: No such file or directory\nb.txt:foo b\n"
    );
}

#[test]
fn rejects_invalid_character_class_names() {
    let cases = [
        ("[[:word:]]", "Invalid character class name"),
        ("[[:ascii:]]", "Invalid character class name"),
        ("[[:^digit:]]", "Invalid character class name"),
        ("[[::]]", "Invalid character class name"),
        ("[[:digit]]", "Unmatched [, [^, [:, [., or [="),
        ("[x[:]y]", "Unmatched [, [^, [:, [., or [="),
        ("[[:]", "Unmatched [, [^, [:, [., or [="),
        ("[[:a:b]", "Unmatched [, [^, [:, [., or [="),
    ];
    for (pattern, message) in cases {
        for mode in ["-G", "-E"] {
            let (_s, mut c) = ucmd();
            c.args(&[mode, pattern])
                .pipe_in("7\n:]\n")
                .fails_with_code(2)
                .no_stdout()
                .stderr_contains(message);
        }
    }
    // The twelve POSIX names keep working.
    let (_s, mut c) = ucmd();
    c.args(&["[[:xdigit:]]"])
        .pipe_in("f\nz\n")
        .succeeds()
        .stdout_only("f\n");
}
