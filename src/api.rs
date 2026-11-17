//! Display-independent diff results for embedding difftastic.
//!
//! Follows the same steps as `diff_file_content` in `main.rs`, so the
//! positions match what `difft` displays for the same input.

use std::path::Path;

use line_numbers::LineNumber;
use typed_arena::Arena;

use crate::diff::changes::ChangeMap;
use crate::diff::shortest_path::{mark_syntax, ExceededGraphLimit};
use crate::diff::sliders::fix_all_sliders;
use crate::diff::unchanged;
use crate::display::context::{all_matched_lines_filled, opposite_positions};
use crate::display::hunks::{matched_lines_indexes_for_hunk, matched_pos_to_hunks, merge_adjacent};
use crate::display::side_by_side::lines_with_novel;
use crate::hash::DftHashSet;
use crate::line_parser;
use crate::lines::{split_on_newlines, MaxLine};
use crate::options::{
    DiffOptions, DEFAULT_BYTE_LIMIT, DEFAULT_GRAPH_LIMIT, DEFAULT_PARSE_ERROR_LIMIT,
};
use crate::parse::guess_language::{guess, language_name, Language};
use crate::parse::syntax::{
    self, init_next_prev, AtomKind, MatchKind, MatchedPos, StringKind, TokenKind,
};
use crate::parse::tree_sitter_parser as tsp;

/// Limits that make difftastic fall back to a line-based diff.
/// Defaults are the same as `difft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Options {
    pub graph_limit: usize,
    pub byte_limit: usize,
    pub parse_error_limit: usize,
    /// Remove `\r` before diffing (`difft --strip-cr`, on by default).
    pub strip_cr: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            graph_limit: DEFAULT_GRAPH_LIMIT,
            byte_limit: DEFAULT_BYTE_LIMIT,
            parse_error_limit: DEFAULT_PARSE_ERROR_LIMIT,
            strip_cr: true,
        }
    }
}

/// Why a file with a known language was diffed line by line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The larger side has `num_bytes` bytes, over `byte_limit`.
    ByteLimit { num_bytes: usize },
    GraphLimit,
    ParseErrorLimit { error_count: usize },
}

/// Same categories as the `highlight` field of `difft --display json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Highlight {
    Delimiter,
    Normal,
    String,
    Type,
    Comment,
    Keyword,
    TreeSitterError,
}

/// A token position on one side. Lines are 0-based; `start`..`end`
/// is a byte range within the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub line: u32,
    pub start: u32,
    pub end: u32,
    pub changed: bool,
    pub highlight: Highlight,
}

/// One row of the side-by-side view: 0-based line numbers, `None` for padding.
pub type LinePair = (Option<u32>, Option<u32>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// `None` means plain text.
    pub language: Option<&'static str>,
    pub fallback: Option<Fallback>,
    pub identical: bool,
    pub lhs: Vec<Token>,
    pub rhs: Vec<Token>,
    /// The whole file aligned as `difft` side-by-side does: every line of
    /// both sides exactly once, in increasing order.
    pub aligned: Vec<LinePair>,
    /// Pairs from `aligned` with a changed line on either side, grouped as
    /// the chunks of `difft --display json`. Empty when either side is
    /// empty, as `difft` reports such files as created or deleted.
    pub hunks: Vec<Vec<LinePair>>,
}

/// Diff two decoded texts. `None` means the side does not exist (like
/// `/dev/null` for `difft`); `path` is only used to guess the language.
pub fn diff(path: &Path, lhs: Option<&str>, rhs: Option<&str>, options: &Options) -> Diff {
    let lhs_src = prepare(lhs.unwrap_or(""), options);
    let rhs_src = prepare(rhs.unwrap_or(""), options);
    let guess_src = if rhs.is_none() { &lhs_src } else { &rhs_src };
    let language = guess(path, guess_src, &[]);

    let mut result = Diff {
        language: language.map(language_name),
        fallback: None,
        identical: lhs_src == rhs_src,
        lhs: vec![],
        rhs: vec![],
        aligned: vec![],
        hunks: vec![],
    };
    if result.identical {
        let num_lines = src_lines(&lhs_src).len() as u32;
        result.aligned = (0..num_lines).map(|n| (Some(n), Some(n))).collect();
        return result;
    }

    let positions = match language {
        Some(language) => syntax_positions(language, &lhs_src, &rhs_src, options),
        None => Ok(line_parser::change_positions(&lhs_src, &rhs_src)),
    };
    let (lhs_positions, rhs_positions) = positions.unwrap_or_else(|fallback| {
        result.fallback = Some(fallback);
        line_parser::change_positions(&lhs_src, &rhs_src)
    });

    result.lhs = lhs_positions.iter().map(token).collect();
    result.rhs = rhs_positions.iter().map(token).collect();
    (result.aligned, result.hunks) = align(&lhs_src, &rhs_src, &lhs_positions, &rhs_positions);
    result
}

fn align(
    lhs_src: &str,
    rhs_src: &str,
    lhs_mps: &[MatchedPos],
    rhs_mps: &[MatchedPos],
) -> (Vec<LinePair>, Vec<Vec<LinePair>>) {
    let lhs_lines = src_lines(lhs_src);
    let rhs_lines = src_lines(rhs_src);
    // `difft` shows an empty side as a single column, without alignment.
    if lhs_lines.is_empty() || rhs_lines.is_empty() {
        let lhs = (0..lhs_lines.len() as u32).map(|n| (Some(n), None));
        let rhs = (0..rhs_lines.len() as u32).map(|n| (None, Some(n)));
        return (lhs.chain(rhs).collect(), vec![]);
    }

    let matched = all_matched_lines_filled(lhs_mps, rhs_mps, &lhs_lines, &rhs_lines);
    let hunks = hunks(lhs_src, rhs_src, lhs_mps, rhs_mps, &matched);
    (matched.iter().map(|&pair| line_pair(pair)).collect(), hunks)
}

// Same trimming as `side_by_side::print`: the final newline does not start a line.
fn src_lines(src: &str) -> Vec<&str> {
    if src.is_empty() {
        return vec![];
    }
    let mut lines: Vec<&str> = split_on_newlines(src).collect();
    if lines.last() == Some(&"") && lines.len() > 1 {
        lines.pop();
    }
    lines
}

// Same hunk boundaries and line filter as `display::json` (no context lines).
fn hunks(
    lhs_src: &str,
    rhs_src: &str,
    lhs_mps: &[MatchedPos],
    rhs_mps: &[MatchedPos],
    matched: &[(Option<LineNumber>, Option<LineNumber>)],
) -> Vec<Vec<LinePair>> {
    let hunks = matched_pos_to_hunks(lhs_mps, rhs_mps);
    let hunks = merge_adjacent(
        &hunks,
        &opposite_positions(lhs_mps),
        &opposite_positions(rhs_mps),
        lhs_src.max_line(),
        rhs_src.max_line(),
        0,
    );
    let (lhs_novel, rhs_novel) = lines_with_novel(lhs_mps, rhs_mps);
    let is_novel = |line: Option<LineNumber>, novel: &DftHashSet<LineNumber>| {
        line.is_some_and(|line| novel.contains(&line))
    };

    let mut rest = matched;
    hunks
        .iter()
        .map(|hunk| {
            let (start, end) = matched_lines_indexes_for_hunk(rest, hunk, 0);
            let lines = rest[start..end]
                .iter()
                .filter(|&&(lhs, rhs)| is_novel(lhs, &lhs_novel) || is_novel(rhs, &rhs_novel))
                .map(|&pair| line_pair(pair))
                .collect();
            rest = &rest[start..];
            lines
        })
        .collect()
}

fn line_pair((lhs, rhs): (Option<LineNumber>, Option<LineNumber>)) -> LinePair {
    (lhs.map(|n| n.0), rhs.map(|n| n.0))
}

// Same preprocessing as `diff_file` in `main.rs`; the final newline makes
// "foo" and "foo\n" compare equal.
fn prepare(src: &str, options: &Options) -> String {
    let mut src = src.to_owned();
    if options.strip_cr {
        src.retain(|c| c != '\r');
    }
    if !src.is_empty() && !src.ends_with('\n') {
        src.push('\n');
    }
    src
}

fn syntax_positions(
    language: Language,
    lhs_src: &str,
    rhs_src: &str,
    options: &Options,
) -> Result<(Vec<MatchedPos>, Vec<MatchedPos>), Fallback> {
    let diff_options = DiffOptions {
        graph_limit: options.graph_limit,
        byte_limit: options.byte_limit,
        parse_error_limit: options.parse_error_limit,
        ..DiffOptions::default()
    };
    let config = tsp::from_language(language);
    let (lhs_tree, rhs_tree) = tsp::to_tree_with_limit(&diff_options, config, lhs_src, rhs_src)
        .map_err(|tsp::ExceededByteLimit(num_bytes)| Fallback::ByteLimit { num_bytes })?;

    let arena = Arena::new();
    let (lhs, rhs) = tsp::to_syntax_with_limit(
        lhs_src,
        rhs_src,
        &lhs_tree,
        &rhs_tree,
        &arena,
        config,
        &diff_options,
    )
    .map_err(|e| Fallback::ParseErrorLimit {
        error_count: e.error_count,
    })?;

    let mut change_map = ChangeMap::default();
    for (lhs_section, rhs_section) in unchanged::mark_unchanged(&lhs, &rhs, &mut change_map) {
        init_next_prev(&lhs_section);
        init_next_prev(&rhs_section);
        mark_syntax(
            lhs_section.first().copied(),
            rhs_section.first().copied(),
            &mut change_map,
            diff_options.graph_limit,
        )
        .map_err(|ExceededGraphLimit {}| Fallback::GraphLimit)?;
    }

    fix_all_sliders(language, &lhs, &mut change_map);
    fix_all_sliders(language, &rhs, &mut change_map);
    Ok((
        syntax::change_positions(&lhs, &change_map),
        syntax::change_positions(&rhs, &change_map),
    ))
}

fn token(mp: &MatchedPos) -> Token {
    Token {
        line: mp.pos.line.0,
        start: mp.pos.start_col,
        end: mp.pos.end_col,
        changed: mp.kind.is_novel(),
        highlight: highlight(&mp.kind),
    }
}

// Mirrors `Highlight::from_match` in `display/json.rs`.
fn highlight(kind: &MatchKind) -> Highlight {
    let token_kind = match kind {
        MatchKind::Ignored { highlight }
        | MatchKind::UnchangedToken { highlight, .. }
        | MatchKind::Novel { highlight }
        | MatchKind::NovelWord { highlight }
        | MatchKind::UnchangedPartOfNovelItem { highlight, .. } => highlight,
    };
    match token_kind {
        TokenKind::Delimiter => Highlight::Delimiter,
        TokenKind::Atom(atom) => match atom {
            AtomKind::String(StringKind::StringLiteral) => Highlight::String,
            AtomKind::String(StringKind::Text) | AtomKind::Normal | AtomKind::CanIgnore => {
                Highlight::Normal
            }
            AtomKind::Keyword => Highlight::Keyword,
            AtomKind::Comment => Highlight::Comment,
            AtomKind::Type => Highlight::Type,
            AtomKind::TreeSitterError => Highlight::TreeSitterError,
        },
    }
}
