//! Read/search fidelity goldens (M1 exit criterion).
//!
//! Fixtures are synthetic (`tests/fixtures/gen_golden.py`, ADR-020). For every search fixture
//! the test checks fidelity invariants independently of the snapshot:
//! - totals are exact (matches and files, known from how the fixture was built);
//! - every shown line is a verbatim raw line (or its text after the path);
//! - shown + omitted + verbatim "other" lines account for every raw line;
//! - the delivered text is smaller than the raw output;
//! - a handle is offered whenever lines were omitted.
//!
//! Read fixtures: the head is a verbatim prefix, every outline range lies inside the file and
//! starts on the line it names, and head + context is smaller than the file.
//!
//! Then each output is compared with its reviewed snapshot (`*.expected`). To accept an intended
//! change: `AAES_UPDATE_GOLDEN=1 cargo test -p aaes-core --test golden`, then review the diff.

use std::path::{Path, PathBuf};

use aaes_core::shape::{ShapeConfig, read_context, shape_read, shape_search};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn snapshot(path: &Path, actual: &str) {
    assert!(
        path.extension().is_some_and(|x| x == "expected"),
        "snapshots go to *.expected files only"
    );
    if std::env::var_os("AAES_UPDATE_GOLDEN").is_some() {
        std::fs::write(path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|_| {
        panic!(
            "missing snapshot {}; run with AAES_UPDATE_GOLDEN=1",
            path.display()
        )
    });
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(e, a)| e != a)
            .unwrap_or(expected.lines().count().min(actual.lines().count()));
        panic!(
            "{} differs from the snapshot at line {}:\n  expected: {:?}\n  actual:   {:?}",
            path.display(),
            line + 1,
            expected.lines().nth(line),
            actual.lines().nth(line)
        );
    }
}

fn raw_lines(text: &str) -> Vec<&str> {
    text.strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .filter(|l| !l.is_empty() && *l != "--")
        .collect()
}

fn inputs(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| keep(p))
        .collect();
    v.sort();
    v
}

#[test]
fn search_goldens() {
    let dir = fixtures().join("search");
    let cfg = ShapeConfig::default();
    let mut checked = 0;
    for path in inputs(&dir, |p| p.extension().is_some_and(|x| x == "txt")) {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&path).unwrap();
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path.with_extension("json")).unwrap())
                .unwrap();
        let cwd = meta["cwd"].as_str();
        let single = meta["single_path"].as_str();
        let result = shape_search(&raw, cwd, &cfg, Some("h_golden"), single);
        let expect = meta["expect"].as_str().unwrap();
        checked += 1;
        if expect != "shaped" {
            let err = result.expect_err(&name);
            assert_eq!(err.reason(), expect, "{name}");
            continue;
        }
        let r = result.unwrap_or_else(|e| panic!("{name}: not shaped ({})", e.reason()));

        // Exact totals.
        assert_eq!(
            r.matches as u64,
            meta["matches"].as_u64().unwrap(),
            "{name}: matches"
        );
        assert_eq!(
            r.files as u64,
            meta["files"].as_u64().unwrap(),
            "{name}: files"
        );
        assert!(
            r.text.contains(&format!("[aaes] {} match", r.matches)),
            "{name}: footer total"
        );

        // Delivered smaller than raw; a handle whenever something was omitted.
        assert!(
            r.text.len() < raw.len(),
            "{name}: delivered must be smaller than raw"
        );
        assert_eq!(
            r.text.contains("aaes show h_golden"),
            r.omitted_lines > 0,
            "{name}: handle offered if and only if lines were omitted"
        );

        // Every shown line is verbatim; every raw line is shown, counted as omitted, or kept
        // as an other line (in original order).
        let raw = raw_lines(&raw);
        let mut shown = 0;
        let mut others: Vec<&str> = Vec::new();
        let mut in_other = false;
        for line in r.text.lines() {
            if let Some(rest) = line.strip_prefix("  ") {
                assert!(
                    raw.iter()
                        .any(|l| *l == rest || (l.ends_with(rest) && l.len() > rest.len())),
                    "{name}: shown line not verbatim: {rest:?}"
                );
                shown += 1;
            } else if line.starts_with("[aaes] ") {
                in_other = line.contains("other line");
            } else if in_other {
                others.push(line);
            }
        }
        let mut pos = 0;
        for o in &others {
            pos += raw[pos..].iter().position(|l| l == o).unwrap_or_else(|| {
                panic!("{name}: other line not verbatim or out of order: {o:?}")
            }) + 1;
        }
        assert_eq!(
            shown + others.len() + r.omitted_lines,
            raw.len(),
            "{name}: shown {shown} + other {} + omitted {} must cover every raw line",
            others.len(),
            r.omitted_lines
        );

        snapshot(&dir.join(format!("{name}.expected")), &r.text);
    }
    assert!(checked >= 11, "only {checked} search fixtures found");
}

#[test]
fn read_goldens() {
    let dir = fixtures().join("read");
    let cfg = ShapeConfig::default();
    let mut checked = 0;
    for path in inputs(&dir, |p| p.extension().is_some_and(|x| x != "expected")) {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&path).unwrap();
        let r = shape_read(&name, &src, &cfg).unwrap_or_else(|| panic!("{name}: not shaped"));
        let total = src.lines().count();
        assert!(
            src.starts_with(&r.head),
            "{name}: head is a verbatim prefix"
        );
        assert_eq!(
            r.head_lines,
            r.head.lines().count(),
            "{name}: head line count"
        );
        assert_eq!(r.total_lines, total, "{name}: total lines");
        let src_lines: Vec<&str> = src.lines().collect();
        let mut items = 0;
        for item in r.outline.lines() {
            let (range, label) = item
                .trim_start()
                .split_once("  ")
                .unwrap_or_else(|| panic!("{name}: no range in {item:?}"));
            let (a, b) = range.split_once('-').unwrap_or((range, range));
            let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
            assert!(
                1 <= a && a <= b && b <= total,
                "{name}: range {a}-{b} of {total}"
            );
            let key = label.trim_end_matches('…').trim();
            let key = key.split(['(', '{', ':', ' ']).next().unwrap_or(key);
            assert!(
                src_lines[a - 1].contains(key),
                "{name}: line {a} does not contain `{key}`: {:?}",
                src_lines[a - 1]
            );
            items += 1;
        }
        assert!(items > 0, "{name}: empty outline");
        let ctx = read_context(&name, &r, total);
        assert!(
            r.head.len() + ctx.len() < src.len(),
            "{name}: delivered must be smaller than raw"
        );
        snapshot(&dir.join(format!("{name}.expected")), &ctx);
        checked += 1;
    }
    assert!(checked >= 3, "only {checked} read fixtures found");
}
