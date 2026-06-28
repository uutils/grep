// This file is part of the uutils grep package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

#[doc(hidden)]
pub mod context_buffer;
#[doc(hidden)]
pub mod line_buffer;
#[doc(hidden)]
pub mod matcher;
mod output;
mod searcher;

use crate::line_buffer::LineBuffer;
use crate::matcher::Matcher;
use crate::output::OutputWriter;
use crate::searcher::Searcher;
use clap::{Arg, ArgAction, Command};
use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal as _, Read};
use std::path::Path;
use uucore::error::{ExitCode, FromIo, UResult, USimpleError};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[doc(hidden)]
pub enum RegexMode {
    Fixed,
    Basic,
    Extended,
    Perl,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum BinaryMode {
    Binary,
    Text,
    WithoutMatch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorMode {
    Always,
    Never,
    Auto,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum DirectoryMode {
    Read,
    Skip,
    Recurse,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum DeviceMode {
    Default,
    Read,
    Skip,
}

#[doc(hidden)]
pub struct ColorConfig<'a> {
    pub matched_selected: &'a str,
    pub matched_context: &'a str,
    pub filename: &'a str,
    pub line_number: &'a str,
    pub byte_offset: &'a str,
    pub separator: &'a str,
    pub selected_line: &'a str,
    pub context_line: &'a str,

    pub reverse_video: bool,
    pub no_erase: bool,
}

#[doc(hidden)]
pub struct GlobSet {
    patterns: Vec<glob::Pattern>,
}

#[doc(hidden)]
pub struct Config<'a> {
    // Searcher
    pub directory_mode: DirectoryMode,
    pub device_mode: DeviceMode,
    pub follow_symlinks: bool,
    pub include_globs: GlobSet,
    pub exclude_globs: GlobSet,
    pub exclude_dir_globs: GlobSet,
    pub label: &'a str,
    #[cfg(windows)]
    pub strip_cr: bool,
    pub binary_mode: BinaryMode,
    pub max_count: Option<u64>,
    pub before_context: usize,
    pub after_context: usize,
    pub has_context: bool,

    // Matcher
    pub patterns: &'a [&'a str],
    pub regex_mode: RegexMode,
    pub ignore_case: bool,
    pub invert_match: bool,
    pub word_regexp: bool,
    pub line_regexp: bool,

    // Output
    pub quiet: bool,
    pub count: bool,
    pub show_filename: bool,
    pub files_with_matches: bool,
    pub files_without_match: bool,
    pub only_matching: bool,
    pub byte_offset: bool,
    pub line_number: bool,
    pub initial_tab: bool,
    pub null_separator: bool,
    pub null_data: bool,
    pub line_buffered: bool,
    pub no_messages: bool,
    pub group_separator: Option<&'a str>,
    pub use_color: bool,
    pub color_config: ColorConfig<'a>,
}

#[uucore::main(no_signals)]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let args = expand_num_shorthand(args);
    let matches = uucore::clap_localization::handle_clap_result_with_exit_code(uu_app(), args, 2)?;

    let grep_color = std::env::var("GREP_COLOR").unwrap_or_default();
    let grep_colors = std::env::var("GREP_COLORS").unwrap_or_default();

    let patterns_or_files: Vec<_> = matches
        .get_many::<OsString>("patterns_or_files")
        .map_or(Default::default(), |v| v.collect());
    let extended_regexp = matches.get_flag("extended_regexp");
    let fixed_strings = matches.get_flag("fixed_strings");
    let basic_regexp = matches.get_flag("basic_regexp");
    let perl_regexp = matches.get_flag("perl_regexp");
    let regexp = matches.get_many::<String>("regexp").unwrap_or_default();
    let file_pattern = matches
        .get_many::<String>("file_pattern")
        .unwrap_or_default();
    let ignore_case = matches.get_flag("ignore_case");
    let word_regexp = matches.get_flag("word_regexp");
    let line_regexp = matches.get_flag("line_regexp");
    let null_data = matches.get_flag("null_data");
    let no_messages = matches.get_flag("no_messages");
    let invert_match = matches.get_flag("invert_match");
    let max_count = matches.get_one::<u64>("max_count").copied();
    let byte_offset = matches.get_flag("byte_offset");
    let line_number = matches.get_flag("line_number");
    let line_buffered = matches.get_flag("line_buffered");
    let with_filename = matches.get_flag("with_filename");
    let no_filename = matches.get_flag("no_filename");
    let label = matches
        .get_one::<String>("label")
        .map_or("(standard input)", |s| s.as_str());
    let only_matching = matches.get_flag("only_matching");
    let quiet = matches.get_flag("quiet");
    let binary_files = matches
        .get_one::<String>("binary_files")
        .map(String::as_str);
    let text = matches.get_flag("text");
    let skip_binary = matches.get_flag("skip_binary");
    let directories = matches.get_one::<String>("directories").map(String::as_str);
    let devices = matches.get_one::<String>("devices").map(String::as_str);
    let recursive = matches.get_flag("recursive");
    let dereference_recursive = matches.get_flag("dereference_recursive");
    let include = matches.get_many::<String>("include").unwrap_or_default();
    let exclude = matches.get_many::<String>("exclude").unwrap_or_default();
    let exclude_from = matches
        .get_many::<String>("exclude_from")
        .unwrap_or_default();
    let exclude_dir = matches
        .get_many::<String>("exclude_dir")
        .unwrap_or_default();
    let files_without_match = matches.get_flag("files_without_match");
    let files_with_matches = matches.get_flag("files_with_matches");
    let count = matches.get_flag("count");
    let initial_tab = matches.get_flag("initial_tab");
    let null = matches.get_flag("null");
    let before_context = matches.get_one::<usize>("before_context").copied();
    let after_context = matches.get_one::<usize>("after_context").copied();
    let context = matches.get_one::<usize>("context").copied();
    let group_separator = matches
        .get_one::<String>("group_separator")
        .map_or("--", |s| s.as_str());
    let no_group_separator = matches.get_flag("no_group_separator");
    let color = matches
        .get_one::<String>("color")
        .map_or("", |s| s.as_str());
    #[cfg(windows)]
    let binary = matches.get_flag("binary");

    let matcher_mode_count = [extended_regexp, fixed_strings, basic_regexp, perl_regexp]
        .into_iter()
        .filter(|matched| *matched)
        .count();
    if matcher_mode_count > 1 {
        return Err(USimpleError::new(
            2,
            "conflicting matchers specified".to_string(),
        ));
    }

    // With -e/-f given, ALL positionals are files.
    let has_explicit_patterns = regexp.len() != 0 || file_pattern.len() != 0;
    let (positional_pattern, file_args) = if has_explicit_patterns {
        (None, &patterns_or_files[..])
    } else {
        patterns_or_files
            .split_first()
            .map_or((None, &[][..]), |(p, rest)| (Some(*p), rest))
    };

    // An empty pattern set is a usage error only when no explicit pattern source was
    // given (`-e` / `-f`). An empty `-f` file is legitimate and simply matches nothing.
    let mut pattern_strings = Vec::new();
    let mut patterns = Vec::new();
    {
        for expr in regexp {
            for line in expr.split('\n') {
                patterns.push(line);
            }
        }

        for path in file_pattern {
            let contents = if *path == "-" {
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err_context(|| "(standard input)".to_string())?;
                buf
            } else {
                std::fs::read_to_string(path).map_err_context(|| path.to_string())?
            };
            pattern_strings.push(contents);
        }
        for contents in &pattern_strings {
            if !contents.is_empty() {
                let body = contents.strip_suffix('\n').unwrap_or(contents);
                for line in body.split('\n') {
                    patterns.push(line);
                }
            }
        }

        if let Some(pos) = positional_pattern {
            let pat = pos
                .to_str()
                .ok_or_else(|| USimpleError::new(2, "pattern must be valid UTF-8".to_string()))?;
            for line in pat.split('\n') {
                patterns.push(line);
            }
        }
    }
    if patterns.is_empty() && !has_explicit_patterns {
        return Err(USimpleError::new(
            2,
            "no PATTERN specified. Try 'grep --help' for more information.".to_string(),
        ));
    }

    // GNU grep's PCRE backend (-P) supports only a single pattern.
    if perl_regexp && patterns.len() > 1 {
        return Err(USimpleError::new(
            2,
            "the -P option only supports a single pattern".to_string(),
        ));
    }

    // Decoded options into enums
    let regex_mode = if fixed_strings {
        RegexMode::Fixed
    } else if extended_regexp {
        RegexMode::Extended
    } else if perl_regexp {
        RegexMode::Perl
    } else {
        RegexMode::Basic
    };
    let directory_mode = if recursive || dereference_recursive {
        DirectoryMode::Recurse
    } else {
        match directories {
            Some("skip") => DirectoryMode::Skip,
            Some("recurse") => DirectoryMode::Recurse,
            _ => DirectoryMode::Read,
        }
    };
    let binary_mode = if text {
        BinaryMode::Text
    } else if skip_binary {
        BinaryMode::WithoutMatch
    } else {
        match binary_files {
            Some("text") => BinaryMode::Text,
            Some("without-match") => BinaryMode::WithoutMatch,
            _ => BinaryMode::Binary,
        }
    };
    let device_mode = match devices {
        Some("read") => DeviceMode::Read,
        Some("skip") => DeviceMode::Skip,
        _ => DeviceMode::Default,
    };
    let color = match color {
        "always" => ColorMode::Always,
        "never" => ColorMode::Never,
        _ => ColorMode::Auto,
    };
    let (before_context, after_context, has_context) = {
        let fallback = context.unwrap_or(0);
        let before = before_context.unwrap_or(fallback);
        let after = after_context.unwrap_or(fallback);
        let has = context.is_some() || before_context.is_some() || after_context.is_some();
        (before, after, has)
    };
    let include_globs = {
        let mut patterns = GlobSet::with_capacity(include.len());
        for pattern in include {
            patterns.add(pattern)?;
        }
        patterns
    };
    let exclude_globs = {
        let mut patterns = GlobSet::with_capacity(exclude.len());
        for pattern in exclude {
            patterns.add(pattern)?;
        }
        for path in exclude_from {
            let contents = std::fs::read_to_string(path).map_err_context(|| path.to_string())?;
            for line in contents.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    patterns.add(trimmed)?;
                }
            }
        }
        patterns
    };
    let exclude_dir_globs = {
        let mut patterns = GlobSet::with_capacity(exclude_dir.len());
        for pattern in exclude_dir {
            patterns.add(pattern)?;
        }
        patterns
    };
    let show_filename = if with_filename {
        true
    } else if no_filename {
        false
    } else {
        match file_args {
            [] => directory_mode == DirectoryMode::Recurse,
            [one] if one.to_str() != Some("-") => {
                directory_mode == DirectoryMode::Recurse && Path::new(one).is_dir()
            }
            [_] => false,
            _ => true,
        }
    };
    let group_separator = (!no_group_separator).then_some(group_separator);
    let use_color = match color {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => std::io::stdout().is_terminal(),
    };
    let color_config = ColorConfig::from_env(&grep_color, &grep_colors);

    let config = Config {
        // Searcher
        directory_mode,
        device_mode,
        follow_symlinks: dereference_recursive,
        include_globs,
        exclude_globs,
        exclude_dir_globs,
        label,
        #[cfg(windows)]
        strip_cr: !binary,
        binary_mode,
        max_count,
        before_context,
        after_context,
        has_context,

        // Matcher
        patterns: &patterns,
        regex_mode,
        ignore_case,
        invert_match,
        word_regexp,
        line_regexp,

        // Output
        quiet,
        count,
        show_filename,
        files_with_matches,
        files_without_match,
        only_matching,
        byte_offset,
        line_number,
        initial_tab,
        null_separator: null,
        null_data,
        line_buffered,
        no_messages,
        group_separator,
        use_color,
        color_config,
    };

    let matcher = Matcher::compile(&config)?;

    // grep with -m 0 should not open any file
    if config.max_count == Some(0) {
        return Err(ExitCode::new(1));
    }

    // An empty pattern matches every line; with `-v`, GNU grep selects no lines
    // and exits as "no match" without reading any input files.
    if invert_match && patterns.iter().any(|pattern| pattern.is_empty()) {
        return Err(ExitCode::new(1));
    }

    let writer = OutputWriter::new(&config);
    let mut searcher = Searcher::new(&config, matcher, writer);
    let mut lb = LineBuffer::new(if config.null_data { b'\0' } else { b'\n' });

    if file_args.is_empty() {
        if directory_mode != DirectoryMode::Recurse {
            _ = searcher.process_stdin(&mut lb);
        } else {
            _ = searcher.process_implicit_cwd(&mut lb);
        }
    } else {
        for f in file_args {
            let cf = if f.to_str() == Some("-") {
                searcher.process_stdin(&mut lb)
            } else {
                searcher.process_path(&mut lb, Path::new(f))
            };
            if cf.is_break() {
                break;
            }
        }
    }

    searcher.finish()
}

pub fn uu_app() -> Command {
    Command::new("grep")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Search for PATTERNS in each FILE.")
        .disable_help_flag(true)
        .disable_version_flag(true)
        // GNU grep accepts repeated options (booleans are idempotent, value
        // options take the last); make clap replace rather than error. Args
        // with ArgAction::Append (e.g. -e/-f/--include) still accumulate.
        .args_override_self(true)
        .after_help(
            "When FILE is '-', read standard input.  If no FILE is given, read standard \
             input, but with -r, recursively search the working directory instead.  With \
             fewer than two FILEs, assume -h.  Exit status is 0 if any line is selected, \
             1 otherwise; if any error occurs and -q is not given, the exit status is 2.",
        )
        .arg(
            Arg::new("patterns_or_files")
                .help("Pattern (if no -e/-f) followed by files to search")
                .index(1)
                .num_args(0..)
                .value_parser(clap::value_parser!(OsString)),
        )
        .arg(
            Arg::new("extended_regexp")
                .short('E')
                .long("extended-regexp")
                .help("PATTERNS are extended regular expressions")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("fixed_strings")
                .short('F')
                .long("fixed-strings")
                .help("PATTERNS are strings")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("basic_regexp")
                .short('G')
                .long("basic-regexp")
                .help("PATTERNS are basic regular expressions")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("perl_regexp")
                .short('P')
                .long("perl-regexp")
                .help("PATTERNS are Perl regular expressions")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("regexp")
                .short('e')
                .long("regexp")
                .value_name("PATTERNS")
                .help("use PATTERNS for matching")
                .action(ArgAction::Append)
                .allow_hyphen_values(true),
        )
        .arg(
            Arg::new("file_pattern")
                .short('f')
                .long("file")
                .value_name("FILE")
                .help("take PATTERNS from FILE")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("ignore_case")
                .short('i')
                .long("ignore-case")
                .short_alias('y')
                .help("ignore case distinctions in patterns and data")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no_ignore_case")
                .long("no-ignore-case")
                .help("do not ignore case distinctions (default)")
                .action(ArgAction::SetTrue)
                .overrides_with("ignore_case"),
        )
        .arg(
            Arg::new("word_regexp")
                .short('w')
                .long("word-regexp")
                .help("match only whole words")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("line_regexp")
                .short('x')
                .long("line-regexp")
                .help("match only whole lines")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("null_data")
                .short('z')
                .long("null-data")
                .help("a data line ends in 0 byte, not newline")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no_messages")
                .short('s')
                .long("no-messages")
                .help("suppress error messages")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("invert_match")
                .short('v')
                .long("invert-match")
                .help("select non-matching lines")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .help("display version information and exit")
                .action(ArgAction::Version),
        )
        .arg(
            Arg::new("help")
                .long("help")
                .help("display this help text and exit")
                .action(ArgAction::Help),
        )
        .arg(
            Arg::new("max_count")
                .short('m')
                .long("max-count")
                .value_name("NUM")
                .help("stop after NUM selected lines")
                .value_parser(clap::value_parser!(u64)),
        )
        .arg(
            Arg::new("byte_offset")
                .short('b')
                .long("byte-offset")
                .help("print the byte offset with output lines")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("line_number")
                .short('n')
                .long("line-number")
                .help("print line number with output lines")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("line_buffered")
                .long("line-buffered")
                .help("flush output on every line")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("with_filename")
                .short('H')
                .long("with-filename")
                .help("print file name with output lines")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("no_filename")
                .short('h')
                .long("no-filename")
                .help("suppress the file name prefix on output")
                .action(ArgAction::SetTrue)
                .overrides_with("with_filename"),
        )
        .arg(
            Arg::new("label")
                .long("label")
                .value_name("LABEL")
                .help("use LABEL as the standard input file name prefix"),
        )
        .arg(
            Arg::new("only_matching")
                .short('o')
                .long("only-matching")
                .help("show only nonempty parts of lines that match")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("quiet")
                .short('q')
                .long("quiet")
                .alias("silent")
                .help("suppress all normal output")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("binary_files")
                .long("binary-files")
                .value_name("TYPE")
                .help("assume that binary files are TYPE; TYPE is 'binary', 'text', or 'without-match'")
                .value_parser(["binary", "text", "without-match"]),
        )
        .arg(
            Arg::new("text")
                .short('a')
                .long("text")
                .help("equivalent to --binary-files=text")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("skip_binary")
                .short('I')
                .help("equivalent to --binary-files=without-match")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("directories")
                .short('d')
                .long("directories")
                .value_name("ACTION")
                .help("how to handle directories; ACTION is 'read', 'recurse', or 'skip'")
                .value_parser(["read", "skip", "recurse"]),
        )
        .arg(
            Arg::new("devices")
                .short('D')
                .long("devices")
                .value_name("ACTION")
                .help("how to handle devices, FIFOs and sockets; ACTION is 'read' or 'skip'")
                .value_parser(["read", "skip"]),
        )
        .arg(
            Arg::new("recursive")
                .short('r')
                .long("recursive")
                .help("like --directories=recurse")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("dereference_recursive")
                .short('R')
                .long("dereference-recursive")
                .help("likewise, but follow all symlinks")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("include")
                .long("include")
                .value_name("GLOB")
                .help("search only files that match GLOB (a file pattern)")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("exclude")
                .long("exclude")
                .value_name("GLOB")
                .help("skip files that match GLOB")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("exclude_from")
                .long("exclude-from")
                .value_name("FILE")
                .help("skip files that match any file pattern from FILE")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("exclude_dir")
                .long("exclude-dir")
                .value_name("GLOB")
                .help("skip directories that match GLOB")
                .action(ArgAction::Append),
        )
        .arg(
            Arg::new("files_without_match")
                .short('L')
                .long("files-without-match")
                .help("print only names of FILEs with no selected lines")
                .action(ArgAction::SetTrue)
                .overrides_with("files_with_matches"),
        )
        .arg(
            Arg::new("files_with_matches")
                .short('l')
                .long("files-with-matches")
                .help("print only names of FILEs with selected lines")
                .action(ArgAction::SetTrue)
                .overrides_with("files_without_match"),
        )
        .arg(
            Arg::new("count")
                .short('c')
                .long("count")
                .help("print only a count of selected lines per FILE")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("initial_tab")
                .short('T')
                .long("initial-tab")
                .help("make tabs line up (if needed)")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("null")
                .short('Z')
                .long("null")
                .help("print 0 byte after FILE name")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("before_context")
                .short('B')
                .long("before-context")
                .value_name("NUM")
                .help("print NUM lines of leading context")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            Arg::new("after_context")
                .short('A')
                .long("after-context")
                .value_name("NUM")
                .help("print NUM lines of trailing context")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            Arg::new("context")
                .short('C')
                .long("context")
                .value_name("NUM")
                .help("print NUM lines of output context")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            Arg::new("group_separator")
                .long("group-separator")
                .value_name("SEP")
                .help("print SEP on line between matches with context")
                .default_value("--"),
        )
        .arg(
            Arg::new("no_group_separator")
                .long("no-group-separator")
                .help("do not print separator for matches with context")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("color")
                .long("color")
                .alias("colour")
                .value_name("WHEN")
                .help("use markers to highlight the matching strings; WHEN is 'always', 'never', or 'auto'")
                .value_parser(["always", "never", "auto"])
                .default_missing_value("auto")
                .num_args(0..=1)
                .require_equals(true),
        )
        .arg(
            Arg::new("binary")
                .short('U')
                .long("binary")
                .help("do not strip CR characters at EOL (MSDOS/Windows)")
                .action(ArgAction::SetTrue),
        )
}

/// Expand GNU grep's `-NUM` shorthand to `-C NUM`.
fn expand_num_shorthand(args: impl Iterator<Item = OsString>) -> Vec<OsString> {
    const SHORT_OPTS_WITH_VALUE: &[u8] = b"efmABCDd";
    const LONG_OPTS_WITH_VALUE: &[&[u8]] = &[
        b"regexp",
        b"file",
        b"max-count",
        b"label",
        b"after-context",
        b"before-context",
        b"context",
        b"devices",
        b"directories",
        b"binary-files",
        b"include",
        b"exclude",
        b"exclude-from",
        b"exclude-dir",
        b"group-separator",
    ];

    fn consumes_next_arg(arg: &[u8]) -> bool {
        if let Some(name) = arg.strip_prefix(b"-") {
            LONG_OPTS_WITH_VALUE.contains(&name)
        } else {
            arg.last()
                .is_some_and(|b| SHORT_OPTS_WITH_VALUE.contains(b))
        }
    }

    let mut out: Vec<OsString> = args.collect();
    let mut i = 1; // argv[0] is the executable name

    while i < out.len() {
        let arg = out[i].as_encoded_bytes();

        // Narrow down to `-flags` (and `--options`) and strip the leading `-` for easier matching.
        let Some(arg) = arg.strip_prefix(b"-") else {
            // Not a flag, skip.
            i += 1;
            continue;
        };

        // No more options after `--`.
        if arg == b"-" {
            break;
        }

        // This flag consumes two args. Skip both.
        if consumes_next_arg(arg) {
            i += 2;
            continue;
        }

        // Translate -NUM to -C NUM.
        if !arg.is_empty() && arg.iter().all(u8::is_ascii_digit) {
            // SAFETY: We know that all `arg` bytes are valid ASCII (digits).
            let digits = unsafe { OsStr::from_encoded_bytes_unchecked(arg) }.to_owned();
            out[i] = OsString::from("-C");
            out.insert(i + 1, digits);
            i += 2;
            continue;
        }

        i += 1;
    }

    out
}

impl Default for GlobSet {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobSet {
    /// Create an empty GlobSet.
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            patterns: Vec::with_capacity(capacity),
        }
    }

    fn add(&mut self, pattern: &str) -> UResult<()> {
        let compiled_pattern = glob::Pattern::new(pattern)
            .map_err(|e| USimpleError::new(2, format!("invalid glob '{pattern}': {e}")))?;
        self.patterns.push(compiled_pattern);
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    fn matches(&self, name: &OsStr) -> bool {
        let Some(name) = name.to_str() else {
            return false;
        };
        self.patterns.iter().any(|pattern| pattern.matches(name))
    }
}

impl<'a> ColorConfig<'a> {
    fn from_env(grep_color: &'a str, grep_colors: &'a str) -> Self {
        let mut config = Self {
            matched_selected: "01;31",
            matched_context: "01;31",
            filename: "35",
            line_number: "32",
            byte_offset: "32",
            separator: "36",
            selected_line: "",
            context_line: "",
            reverse_video: false,
            no_erase: false,
        };

        if !grep_color.is_empty() {
            config.matched_selected = grep_color;
            config.matched_context = grep_color;
        }

        for item in grep_colors.split(':') {
            if let Some((key, value)) = item.split_once('=') {
                match key {
                    "ms" => config.matched_selected = value,
                    "mc" => config.matched_context = value,
                    "fn" => config.filename = value,
                    "ln" => config.line_number = value,
                    "bn" => config.byte_offset = value,
                    "se" => config.separator = value,
                    "sl" => config.selected_line = value,
                    "cx" => config.context_line = value,
                    "rv" => config.reverse_video = true,
                    "ne" => config.no_erase = true,
                    _ => {}
                }
            } else {
                match item {
                    "rv" => config.reverse_video = true,
                    "ne" => config.no_erase = true,
                    _ => {}
                }
            }
        }

        config
    }
}
