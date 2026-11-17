use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use difftastic::api::{diff, Diff, Fallback, Highlight, LinePair, Options};

const JS_LHS: &str = "function greet(name) {\n  return \"Hello, \" + name;\n}\n";
const JS_RHS: &str =
    "function greet(name) {\n  const greeting = \"Hi, \";\n  return greeting + name;\n}\n";

fn js_diff(options: &Options) -> Diff {
    diff(Path::new("greet.js"), Some(JS_LHS), Some(JS_RHS), options)
}

#[test]
fn javascript_tokens_have_changes_and_highlights() {
    let result = js_diff(&Options::default());

    assert_eq!(result.language, Some("JavaScript"));
    assert_eq!(result.fallback, None);
    assert!(!result.identical);
    let tokens = || result.lhs.iter().chain(&result.rhs);
    assert!(tokens().any(|t| t.changed));
    assert!(tokens().any(|t| t.highlight == Highlight::Keyword));
    assert!(tokens().any(|t| t.highlight == Highlight::String));
}

#[test]
fn small_byte_limit_falls_back_to_lines() {
    let options = Options {
        byte_limit: 10,
        ..Options::default()
    };
    let result = js_diff(&options);

    assert_eq!(result.language, Some("JavaScript"));
    assert!(matches!(result.fallback, Some(Fallback::ByteLimit { .. })));
    assert!(result.rhs.iter().any(|t| t.changed));
}

#[test]
fn identical_input_is_identical() {
    let result = diff(
        Path::new("greet.js"),
        Some(JS_LHS),
        Some(JS_LHS),
        &Options::default(),
    );

    assert!(result.identical);
    assert!(result.lhs.is_empty() && result.rhs.is_empty());
}

#[test]
fn unknown_extension_is_plain_text() {
    let result = diff(
        Path::new("notes.unknownext"),
        Some("a\nb\n"),
        Some("a\nc\n"),
        &Options::default(),
    );

    assert_eq!(result.language, None);
    assert_eq!(result.fallback, None);
    assert!(result.rhs.iter().any(|t| t.changed && t.line == 1));
}

struct Sample {
    name: String,
    lhs_path: PathBuf,
    rhs_path: PathBuf,
    lhs: String,
    rhs: String,
}

impl Sample {
    fn diff(&self) -> Diff {
        diff(
            &self.rhs_path,
            Some(&self.lhs),
            Some(&self.rhs),
            &Options::default(),
        )
    }
}

fn samples() -> Vec<Sample> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample_files");
    let mut samples: Vec<Sample> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| {
            let lhs_path = entry.unwrap().path();
            let name = lhs_path.file_name()?.to_str()?.to_owned();
            let rhs_path = dir.join(name.replacen("_1.", "_2.", 1));
            if !name.contains("_1.") || !rhs_path.is_file() {
                return None;
            }
            Some(Sample {
                lhs: decode(&std::fs::read(&lhs_path).unwrap()),
                rhs: decode(&std::fs::read(&rhs_path).unwrap()),
                name,
                lhs_path,
                rhs_path,
            })
        })
        .collect();
    samples.sort_by(|a, b| a.name.cmp(&b.name));
    assert!(!samples.is_empty());
    samples
}

// The text branches of `guess_content`, so the API sees what `difft` decodes.
fn decode(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    if let [0xfe, 0xff, ..] | [0xff, 0xfe, ..] = bytes {
        let big_endian = bytes[0] == 0xfe;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|b| match big_endian {
                true => u16::from_be_bytes([b[0], b[1]]),
                false => u16::from_le_bytes([b[0], b[1]]),
            })
            .collect();
        return String::from_utf16(&units).unwrap();
    }
    let lossy = String::from_utf8_lossy(bytes);
    let invalid = lossy
        .chars()
        .filter(|&c| c == char::REPLACEMENT_CHARACTER || c == '\0')
        .count();
    if invalid <= 2 {
        return lossy.into_owned();
    }
    encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned()
}

// `difft` removes `\r` and treats a missing final newline as present.
fn num_lines(text: &str) -> u32 {
    let text: String = text.chars().filter(|&c| c != '\r').collect();
    let unterminated = !text.is_empty() && !text.ends_with('\n');
    (text.matches('\n').count() + usize::from(unterminated)) as u32
}

fn side(pairs: &[LinePair], pick: fn(&LinePair) -> Option<u32>) -> Vec<u32> {
    pairs.iter().filter_map(pick).collect()
}

#[test]
fn aligned_lines_cover_every_line_once_in_order() {
    for sample in samples() {
        let aligned = sample.diff().aligned;

        let expected_lhs: Vec<u32> = (0..num_lines(&sample.lhs)).collect();
        let expected_rhs: Vec<u32> = (0..num_lines(&sample.rhs)).collect();
        assert_eq!(side(&aligned, |p| p.0), expected_lhs, "{} lhs", sample.name);
        assert_eq!(side(&aligned, |p| p.1), expected_rhs, "{} rhs", sample.name);
    }
}

// These chunks include rows without changes: `display::json` treats a missing
// side as line 0, so a row is kept whenever line 0 of the other side changed.
const JSON_LINE_ZERO_QUIRK: [&str; 6] = [
    "ada_1.adb",
    "elm_1.elm",
    "fortran_1.f90",
    "html_1.html",
    "newick_1.nwk",
    "racket_1.rkt",
];

fn line_pair(value: &serde_json::Value) -> LinePair {
    let number = |v: &serde_json::Value| v.as_u64().map(|n| n as u32);
    (number(&value[0]), number(&value[1]))
}

// Chunk rows in alignment order (the JSON lists them sorted by line numbers).
fn difft_chunks(sample: &Sample) -> Vec<Vec<LinePair>> {
    let output = Command::new(env!("CARGO_BIN_EXE_difft"))
        .env("DFT_UNSTABLE", "yes")
        .args(["--display", "json"])
        .args([&sample.lhs_path, &sample.rhs_path])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let aligned: Vec<LinePair> = json["aligned_lines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(line_pair)
        .collect();
    let number =
        |line: &serde_json::Value, side: &str| line[side]["line_number"].as_u64().map(|n| n as u32);
    let chunks = json["chunks"].as_array().into_iter().flatten();
    chunks
        .map(|chunk| {
            let mut rows: Vec<LinePair> = chunk
                .as_array()
                .unwrap()
                .iter()
                .map(|line| (number(line, "lhs"), number(line, "rhs")))
                .collect();
            rows.sort_by_key(|row| aligned.iter().position(|a| a == row).unwrap());
            rows
        })
        .collect()
}

fn line_sets(hunks: &[Vec<LinePair>]) -> (BTreeSet<u32>, BTreeSet<u32>) {
    let rows = hunks.concat();
    (
        side(&rows, |p| p.0).into_iter().collect(),
        side(&rows, |p| p.1).into_iter().collect(),
    )
}

fn hunk_mismatch(sample: &Sample) -> Option<String> {
    let api = sample.diff().hunks;
    let difft = difft_chunks(sample);
    let bounds = |hunks: &[Vec<LinePair>]| -> Vec<_> {
        hunks
            .iter()
            .map(|h| (h.first().copied(), h.last().copied()))
            .collect()
    };
    if bounds(&api) != bounds(&difft) {
        return Some(format!(
            "{} bounds: api {:?} difft {:?}",
            sample.name,
            bounds(&api),
            bounds(&difft)
        ));
    }
    let quirk = JSON_LINE_ZERO_QUIRK.contains(&sample.name.as_str());
    if !quirk && line_sets(&api) != line_sets(&difft) {
        return Some(format!(
            "{} lines: api {:?} difft {:?}",
            sample.name,
            line_sets(&api),
            line_sets(&difft)
        ));
    }
    None
}

#[test]
fn hunks_match_difft_json_chunks() {
    let mismatches: Vec<String> = samples().iter().filter_map(hunk_mismatch).collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
