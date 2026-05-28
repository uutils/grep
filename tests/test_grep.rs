use std::sync::atomic::{AtomicBool, Ordering};
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

#[test]
fn bre_default_metacharacters() {
    // BRE: . * ^ $ [] [^] and literal +, |, (, )
    let cases: &[(&str, &str, &str)] = &[
        ("a.c", "abc\nadc\nac\n", "abc\nadc\n"),
        ("fo*", "f\nfo\nfoo\nbar\n", "f\nfo\nfoo\n"),
        ("^foo", "foo\nbarfoo\nfoo\n", "foo\nfoo\n"),
        ("bar$", "bar\nfoobar\nbarx\n", "bar\nfoobar\n"),
        ("[Hh]i", "Hi\nhi\nHI\n", "Hi\nhi\n"),
        ("[^a-z]X", "aX\nbX\n.X\n", ".X\n"),
        // `+`, `|`, `(`, `)` are literals in BRE
        ("a+b", "a+b\nab\n", "a+b\n"),
        ("a|b", "a|b\na\n", "a|b\n"),
        ("(x)", "(x)\nx\n", "(x)\n"),
    ];
    for (pat, input, expected) in cases {
        let (_s, mut c) = ucmd();
        c.args(&[pat])
            .pipe_in(*input)
            .succeeds()
            .stdout_only(*expected);
    }
}

#[test]
fn bre_gnu_extensions() {
    // \+ \? \| \{m,n\} \< \> \b \w plus backreferences and leading `*`.
    let (_s, mut c) = ucmd();
    c.args(&[r"o\+"])
        .pipe_in("o\noo\nx\n")
        .succeeds()
        .stdout_only("o\noo\n");

    let (_s, mut c) = ucmd();
    c.args(&[r"Hi\|HI"])
        .pipe_in("Hi\nHI\nhi\n")
        .succeeds()
        .stdout_only("Hi\nHI\n");

    let (_s, mut c) = ucmd();
    c.args(&[r"a\{2,3\}"])
        .pipe_in("a\naa\naaaa\n")
        .succeeds()
        .stdout_only("aa\naaaa\n");

    let (_s, mut c) = ucmd();
    c.args(&[r"\<word\>"])
        .pipe_in("word\nwording\nthe word here\n")
        .succeeds()
        .stdout_only("word\nthe word here\n");

    let (_s, mut c) = ucmd();
    c.args(&[r"\bcontain\b"])
        .pipe_in("contain\ncontainer\ncontained\n")
        .succeeds()
        .stdout_only("contain\n");

    // BRE backreference: repeated adjacent word.
    let (_s, mut c) = ucmd();
    c.args(&[r"\(\b\w\+\b\) \1"])
        .pipe_in("the the cat\nfoo bar\nis is great\n")
        .succeeds()
        .stdout_only("the the cat\nis is great\n");

    // Leading `*` is literal in BRE.
    let (_s, mut c) = ucmd();
    c.args(&["*foo"])
        .pipe_in("*foo\nfoo\n**foo\n")
        .succeeds()
        .stdout_only("*foo\n**foo\n");
}

#[test]
fn ere_metacharacters() {
    let cases: &[(&[&str], &str, &str)] = &[
        (&["-E", "Hi|HI"], "Hi\nHI\nhi\n", "Hi\nHI\n"),
        (&["-E", "o+"], "o\noo\nx\n", "o\noo\n"),
        (
            &["-E", "colou?r"],
            "colour\ncolor\ncolouur\n",
            "colour\ncolor\n",
        ),
        (&["-E", "(foo|bar)x"], "foox\nbarx\nbaz\n", "foox\nbarx\n"),
        (&["-E", "o{2}"], "o\noo\nooo\n", "oo\nooo\n"),
        (
            &["-E", "a{,2}b"],
            "b\nab\naab\naaab\n",
            "b\nab\naab\naaab\n",
        ),
        // ERE backreference works on the Oniguruma path.
        (
            &["-E", r"(....).*\1"],
            "beriberi\nhelloworld\n",
            "beriberi\n",
        ),
    ];
    for (args, input, expected) in cases {
        let (_s, mut c) = ucmd();
        c.args(args)
            .pipe_in(*input)
            .succeeds()
            .stdout_only(*expected);
    }
}

#[test]
fn ere_invalid_pattern_is_error() {
    let (_s, mut c) = ucmd();
    c.args(&["-E", "["])
        .fails_with_code(2)
        .stderr_contains("invalid pattern");
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
}

#[test]
fn line_regexp() {
    let (_s, mut c) = ucmd();
    c.args(&["-x", "foo bar"])
        .pipe_in("foo bar\nfoo bar!\nx foo bar\n")
        .succeeds()
        .stdout_only("foo bar\n");
}

#[test]
fn max_count() {
    // Basic cap.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "2", "a"])
        .pipe_in("a\na\na\na\n")
        .succeeds()
        .stdout_only("a\na\n");

    // -m 0 means zero. Exit 1, no output.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "0", "a"])
        .pipe_in("a\n")
        .fails_with_code(1)
        .no_stdout();

    // -A trailing context still printed after the cutoff.
    let (_s, mut c) = ucmd();
    c.args(&["-m", "1", "-A", "2", "match"])
        .pipe_in("noise\nmatch\nctx1\nctx2\ntail\n")
        .succeeds()
        .stdout_only("match\nctx1\nctx2\n");

    // -c is capped by -m.
    let (_s, mut c) = ucmd();
    c.args(&["-c", "-m", "1", "a"])
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

#[test]
fn empty_pattern_file_matches_nothing() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("empty", "");
    c.args(&["-f", "empty"])
        .pipe_in("anything\n")
        .fails_with_code(1)
        .no_stdout();
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

    // -l early-exits after the first match. Verify it doesn't print twice.
    // when the file has many.
    let (scene, mut c) = ucmd();
    scene.fixtures.write("many", "x\nx\nx\nx\nx\n");
    c.args(&["-l", "x", "many"])
        .succeeds()
        .stdout_only("many\n");
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

    // After a match ends, ^ must not re-match at that position.
    let (_s, mut c) = ucmd();
    c.args(&["-o", "^hello*"])
        .pipe_in("hellooo_hello\n")
        .succeeds()
        .stdout_only("hellooo\n");
}

#[test]
fn quiet_modes() {
    // Match: exit 0, no output.
    let (_s, mut c) = ucmd();
    c.args(&["-q", "a"]).pipe_in("a\n").succeeds().no_output();

    // No match: exit 1, no output.
    let (_s, mut c) = ucmd();
    c.args(&["-q", "z"])
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
}

#[test]
fn null_filename_separator() {
    let (scene, mut c) = ucmd();
    scene.fixtures.write("f", "x\n");
    c.args(&["-Z", "-l", "x", "f"])
        .succeeds()
        .stdout_only("f\0");
}

#[test]
fn after_before_combined_context() {
    let input = "a\nb\nMATCH\nc\nd\n";

    let (_s, mut c) = ucmd();
    c.args(&["-A", "1", "MATCH"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("MATCH\nc\n");

    let (_s, mut c) = ucmd();
    c.args(&["-B", "1", "MATCH"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("b\nMATCH\n");

    let (_s, mut c) = ucmd();
    c.args(&["-C", "1", "MATCH"])
        .pipe_in(input)
        .succeeds()
        .stdout_only("b\nMATCH\nc\n");
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

#[test]
fn color_line_number_uses_green() {
    let (_s, mut c) = ucmd();
    c.args(&["--color=always", "-n", "foo"])
        .pipe_in("foo\n")
        .succeeds()
        .stdout_contains("\x1b[32m\x1b[K1\x1b[m\x1b[K");
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

#[test]
fn invalid_utf8_on_nonmatching_line_does_not_poison_file() {
    // Whether the bad byte appears before or after the match, the matching
    // line must still be returned and the file must not be reported as binary.
    let (scene, _) = ucmd();
    scene.fixtures.write_bytes("before", b"x\x9dy\nhit\n");
    scene.fixtures.write_bytes("after", b"hit\nx\x9dy\n");

    for name in ["before", "after"] {
        let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
        c.args(&["hit", name])
            .succeeds()
            .stdout_is("hit\n")
            .no_stderr();
    }
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

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-I", "hit", "b"]).fails_with_code(1).no_output();

    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["--binary-files=without-match", "hit", "b"])
        .fails_with_code(1)
        .no_output();
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

    // exclude-dir filters whole directories.
    let mut c = scene.cmd(env!("CARGO_BIN_EXE_grep"));
    c.args(&["-r", "-l", "--exclude-dir=sub", "grep", "tree"])
        .succeeds()
        .stdout_does_not_contain("c.txt");

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

#[cfg(unix)]
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

#[test]
fn nonexistent_file_is_error() {
    let (_s, mut c) = ucmd();
    c.args(&["x", "does-not-exist"])
        .fails_with_code(2)
        .stderr_contains("does-not-exist");
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

    // Counting works under -z.
    let (_s, mut c) = ucmd();
    c.args(&["-z", "-c", "hello"])
        .pipe_in(&b"hello\0world\0"[..])
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

    // Repeated value options take the last value (here: -m 1 wins).
    let (_s, mut c) = ucmd();
    c.args(&["-m", "5", "-m", "1", "x"])
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
