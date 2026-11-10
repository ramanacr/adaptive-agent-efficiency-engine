//! Output shaping (FUN-025 / ADR-022 read shaping, FUN-026 / ADR-023 search shaping).
//!
//! Shaping never paraphrases. Read shaping keeps the first lines verbatim and adds an outline
//! with exact line ranges. Search shaping regroups matches by file, keeps match lines verbatim
//! and keeps totals exact. Anything omitted is stated explicitly and can be retrieved.

use crate::outline::{self, Lang};

#[derive(Debug, Clone)]
pub struct ShapeConfig {
    /// Bytes per token for estimates (validated ≈ 2.4 for Claude on tool output; M-1).
    pub bytes_per_token: f64,
    /// Whole-file reads below this many estimated tokens pass through.
    pub read_threshold_tokens: usize,
    pub head_lines: usize,
    pub head_bytes: usize,
    pub outline_max_items: usize,
    pub outline_max_bytes: usize,
    /// Search outputs below this many estimated tokens pass through.
    pub search_threshold_tokens: usize,
    pub search_max_lines_per_file: usize,
    /// Without capping, shape only if it saves at least this fraction.
    pub search_min_saving: f64,
    /// Shape a search output only if at least this fraction of its lines parse as `path:…`
    /// (chained commands often mix in single-file `grep -n` lines or notices; those are kept
    /// verbatim, in order, after the grouped part).
    pub search_min_parsed: f64,
}

impl Default for ShapeConfig {
    fn default() -> Self {
        Self {
            bytes_per_token: 2.4,
            read_threshold_tokens: 2_000,
            head_lines: 40,
            head_bytes: 2_400,
            outline_max_items: 400,
            outline_max_bytes: 10_000,
            search_threshold_tokens: 1_500,
            search_max_lines_per_file: 20,
            search_min_saving: 0.15,
            search_min_parsed: 0.7,
        }
    }
}

impl ShapeConfig {
    pub fn est_tokens(&self, bytes: usize) -> usize {
        (bytes as f64 / self.bytes_per_token).round() as usize
    }
}

// ---------------------------------------------------------------------------------------------
// Read shaping
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ReadShape {
    /// First lines of the file, verbatim (line endings preserved).
    pub head: String,
    pub head_lines: usize,
    pub total_lines: usize,
    /// Rendered outline (indented `start-end  label` lines).
    pub outline: String,
    pub outline_items: usize,
    pub omitted_items: usize,
    pub raw_tokens: usize,
}

/// Shape a whole-file read, or `None` if it should pass through (small, binary, or no gain).
pub fn shape_read(path: &str, content: &str, cfg: &ShapeConfig) -> Option<ReadShape> {
    let raw_tokens = cfg.est_tokens(content.len());
    if raw_tokens < cfg.read_threshold_tokens || content.contains('\0') {
        return None;
    }
    let mut head = String::new();
    let mut head_lines = 0;
    for line in content.split_inclusive('\n') {
        if head_lines >= cfg.head_lines || head.len() + line.len() > cfg.head_bytes {
            break;
        }
        head.push_str(line);
        head_lines += 1;
    }
    let total_lines = content.lines().count();
    let items = outline::outline(outline::lang_for_path(path), content);
    let (outline, omitted_items) =
        outline::render(&items, cfg.outline_max_items, cfg.outline_max_bytes);
    // Not worth it if head + outline (+ ~300 bytes of instructions) keep most of the file.
    if (head.len() + outline.len() + 300) as f64 > content.len() as f64 * 0.7 {
        return None;
    }
    Some(ReadShape {
        head,
        head_lines,
        total_lines,
        outline,
        outline_items: items.len(),
        omitted_items,
        raw_tokens,
    })
}

fn fmt_thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn omitted_note(r: &ReadShape) -> String {
    if r.omitted_items > 0 {
        format!(" ({} deeper items not listed)", r.omitted_items)
    } else {
        String::new()
    }
}

/// Context message delivered next to a shaped Read result (the result itself holds the head lines).
/// `total_lines` is the host's count (may exceed what was read if the host truncated).
pub fn read_context(path: &str, r: &ReadShape, total_lines: usize) -> String {
    format!(
        "[aaes] Large file: the Read result above shows only lines 1-{head} of {total} of {path} (~{tok} tokens). Structural outline with line ranges{omit}:\n{outline}To see exact text, Read the ranges you need with offset/limit (e.g. offset={ex_off}, limit=60). Reading the whole file again returns it in full.",
        head = r.head_lines,
        total = fmt_thousands(total_lines.max(r.total_lines)),
        tok = fmt_thousands(r.raw_tokens),
        omit = omitted_note(r),
        outline = r.outline,
        ex_off = (r.head_lines + 1).min(total_lines.max(1)),
    )
}

/// Replacement stdout for a shell whole-file read (`cat`, `type`, `Get-Content`).
pub fn shell_read_text(path: &str, r: &ReadShape) -> String {
    let mut head = r.head.clone();
    if !head.ends_with('\n') && !head.is_empty() {
        head.push('\n');
    }
    format!(
        "[aaes] Large file {path}: {total} lines (~{tok} tokens). Showing lines 1-{h} verbatim, then a structural outline.\n{head}[aaes] --- outline (line ranges){omit} ---\n{outline}[aaes] For exact text use the Read tool with offset/limit; running this exact command again returns the full output.\n",
        total = fmt_thousands(r.total_lines),
        tok = fmt_thousands(r.raw_tokens),
        h = r.head_lines,
        omit = omitted_note(r),
        outline = r.outline,
    )
}

// ---------------------------------------------------------------------------------------------
// Search shaping
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct SearchShape {
    pub text: String,
    /// Match lines (excludes context lines), exact.
    pub matches: usize,
    pub files: usize,
    /// Lines (match + context) left out because of the per-file cap.
    pub omitted_lines: usize,
    pub raw_tokens: usize,
}

/// Why a search result was passed through unchanged (diagnostics only; content-free).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSkip {
    /// Below `search_threshold_tokens`.
    Small,
    /// Fewer than `search_min_parsed` of the lines look like `path:…` grep output.
    Unparsed {
        lines: usize,
        parsed: usize,
        /// Unparsed lines by [`line_form`], most common first.
        forms: Vec<(&'static str, usize)>,
    },
    /// Grouping would not save at least `search_min_saving`.
    LowSaving,
}

impl SearchSkip {
    pub fn reason(&self) -> &'static str {
        match self {
            SearchSkip::Small => "small",
            SearchSkip::Unparsed { .. } => "unparsed",
            SearchSkip::LowSaving => "low_saving",
        }
    }
}

/// Coarse, content-free form of a search output line we could not parse, to see which output
/// formats are worth supporting next.
pub fn line_form(line: &str) -> &'static str {
    let l = line.trim_start();
    let digits = l.chars().take_while(char::is_ascii_digit).count();
    if l.starts_with('{') || l.starts_with('[') {
        "json"
    } else if digits > 0 && matches!(l.as_bytes().get(digits), Some(b':' | b'-')) {
        "lineno_only" // single-file `grep -n`, or the body of `rg --heading`
    } else if digits > 0 && digits == l.len() {
        "number"
    } else if l.starts_with("Binary file") || l.contains("binary file matches") {
        "binary_notice"
    } else if plausible_path(l.trim_end()) && !without_drive(l).contains(':') {
        "path_only" // `grep -l`, or the file line of `rg --heading`
    } else if l.contains(':') {
        "colon_other"
    } else {
        "other"
    }
}

/// `l` without a leading drive prefix (`C:`). Slices only after two ASCII bytes, so it never cuts
/// a multi-byte character (a UTF-8 BOM, non-Latin path).
fn without_drive(l: &str) -> &str {
    let b = l.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        &l[2..]
    } else {
        l
    }
}

struct Parsed<'a> {
    path: &'a str,
    rest: &'a str,
    is_match: bool,
}

fn plausible_path(p: &str) -> bool {
    !p.is_empty()
        && !p.chars().all(|c| c.is_ascii_digit())
        && (p.contains('.') || p.contains('/') || p.contains('\\'))
        && !p.contains("  ")
}

/// Split `path:12:text`, `path:text` (match) or `path-12-text` (context, rg -C). Handles `C:\…`.
fn parse_line(line: &str) -> Option<Parsed<'_>> {
    let b = line.as_bytes();
    let skip = if b.len() > 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
    {
        2
    } else {
        0
    };
    // match line: first ':' after the drive prefix
    if let Some(p) = line[skip..].find(':').map(|p| p + skip) {
        let path = &line[..p];
        if plausible_path(path) {
            return Some(Parsed {
                path,
                rest: &line[p + 1..],
                is_match: true,
            });
        }
    }
    // context line: `path-<digits>-text`
    let mut search_from = skip;
    while let Some(p) = line[search_from..].find('-').map(|p| p + search_from) {
        let digits: usize = line[p + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits > 0 && line[p + 1 + digits..].starts_with('-') && plausible_path(&line[..p]) {
            return Some(Parsed {
                path: &line[..p],
                rest: &line[p + 1..],
                is_match: false,
            });
        }
        search_from = p + 1;
    }
    None
}

/// `12:text` (match) or `12-text` (context): single-file `grep -n` output, or the Grep tool on
/// one file, which prints no path. `path` labels the group.
fn parse_lineno<'a>(line: &'a str, path: &'a str) -> Option<Parsed<'a>> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let sep = *line.as_bytes().get(digits)?;
    if digits == 0 || !(sep == b':' || sep == b'-') {
        return None;
    }
    Some(Parsed {
        path,
        rest: line,
        is_match: sep == b':',
    })
}

/// `path` relative to `cwd` when inside it (as search output shows paths).
pub fn display_rel<'a>(path: &'a str, cwd: Option<&str>) -> &'a str {
    relativize(path, cwd)
}

fn relativize<'a>(path: &'a str, cwd: Option<&str>) -> &'a str {
    let Some(cwd) = cwd else { return path };
    let norm = |s: &str| s.replace('\\', "/").to_ascii_lowercase();
    let (np, nc) = (norm(path), norm(cwd.trim_end_matches(['/', '\\'])));
    if np.len() > nc.len() + 1 && np.starts_with(&nc) && np.as_bytes()[nc.len()] == b'/' {
        &path[nc.len() + 1..]
    } else {
        path
    }
}

/// Shape grep/rg-style output, or `None` to pass through. `handle` is mentioned in the footer
/// only when lines were omitted (the caller must then persist the raw output under it).
/// `single_path` labels output without paths (single-file `grep -n`); `None` means unknown.
pub fn shape_search<'a>(
    text: &'a str,
    cwd: Option<&str>,
    cfg: &ShapeConfig,
    handle: Option<&str>,
    single_path: Option<&'a str>,
) -> Result<SearchShape, SearchSkip> {
    let raw_tokens = cfg.est_tokens(text.len());
    if raw_tokens < cfg.search_threshold_tokens {
        return Err(SearchSkip::Small);
    }
    // A UTF-8 BOM (PowerShell output) would otherwise become part of the first path.
    let lines: Vec<&str> = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .lines()
        .filter(|l| !l.is_empty() && *l != "--")
        .collect();
    let enough =
        |n: usize| !lines.is_empty() && n as f64 >= lines.len() as f64 * cfg.search_min_parsed;
    let mut parsed: Vec<Option<Parsed>> = lines.iter().map(|l| parse_line(l)).collect();
    let mut ok = parsed.iter().filter(|p| p.is_some()).count();
    if !enough(ok) {
        // Output without paths: one file, `12:text` lines.
        let label = single_path.unwrap_or("(single file)");
        let by_lineno: Vec<Option<Parsed>> = lines.iter().map(|l| parse_lineno(l, label)).collect();
        let n = by_lineno.iter().filter(|p| p.is_some()).count();
        if enough(n) {
            parsed = by_lineno;
            ok = n;
        }
    }
    if !enough(ok) {
        // not grep-shaped (e.g. single-file output without paths): record what it looked like
        let mut forms: Vec<(&'static str, usize)> = Vec::new();
        for l in lines
            .iter()
            .zip(&parsed)
            .filter(|(_, p)| p.is_none())
            .map(|(l, _)| l)
        {
            let f = line_form(l);
            match forms.iter_mut().find(|(k, _)| *k == f) {
                Some((_, n)) => *n += 1,
                None => forms.push((f, 1)),
            }
        }
        forms.sort_by_key(|f| std::cmp::Reverse(f.1));
        return Err(SearchSkip::Unparsed {
            lines: lines.len(),
            parsed: ok,
            forms,
        });
    }

    // group by path, keep first-appearance order
    let mut order: Vec<&str> = Vec::new();
    let mut groups: std::collections::HashMap<&str, Vec<&Parsed>> =
        std::collections::HashMap::new();
    for p in parsed.iter().flatten() {
        let g = groups.entry(p.path).or_default();
        if g.is_empty() {
            order.push(p.path);
        }
        g.push(p);
    }
    let matches = parsed.iter().flatten().filter(|p| p.is_match).count();
    let mut out = String::new();
    let mut omitted = 0;
    let mut capped_files = 0;
    for path in &order {
        let g = &groups[path];
        let m = g.iter().filter(|p| p.is_match).count();
        let shown = g.len().min(cfg.search_max_lines_per_file);
        let cap_note = if shown < g.len() {
            capped_files += 1;
            omitted += g.len() - shown;
            format!(", showing first {shown} lines")
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{} ({m} match{}{cap_note})\n",
            relativize(path, cwd),
            if m == 1 { "" } else { "es" }
        ));
        for p in g.iter().take(shown) {
            out.push_str("  ");
            out.push_str(p.rest);
            out.push('\n');
        }
    }
    // lines we could not parse (e.g. from a chained single-file `grep -n`) are kept verbatim,
    // in their original order, after the grouped part
    let unparsed: Vec<&&str> = lines
        .iter()
        .zip(&parsed)
        .filter(|(_, p)| p.is_none())
        .map(|(l, _)| l)
        .collect();
    if !unparsed.is_empty() {
        out.push_str(&format!(
            "[aaes] {} other line{} (not `path:line` form), verbatim in original order:\n",
            unparsed.len(),
            if unparsed.len() == 1 { "" } else { "s" }
        ));
    }
    for l in &unparsed {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&format!(
        "[aaes] {matches} match{} in {} file{}; each path shown once{}.",
        if matches == 1 { "" } else { "es" },
        order.len(),
        if order.len() == 1 { "" } else { "s" },
        match cwd {
            Some(c) => format!(", relative to {c}"),
            None => String::new(),
        }
    ));
    if omitted > 0 {
        out.push_str(&format!(
            " {omitted} lines omitted from {capped_files} file{} (cap {} per file)",
            if capped_files == 1 { "" } else { "s" },
            cfg.search_max_lines_per_file
        ));
        match handle {
            Some(h) => out.push_str(&format!(
                "; full output: `aaes show {h}` (supports --grep, --lines)"
            )),
            None => out.push_str("; narrow the search path or pattern to see them"),
        }
        out.push('.');
    }
    out.push('\n');

    // Delivered output is never larger than the raw output (M1 exit criterion), even when lines
    // were capped; without capping it must also save at least `search_min_saving`.
    if out.len() >= text.len()
        || (omitted == 0 && (out.len() as f64) > text.len() as f64 * (1.0 - cfg.search_min_saving))
    {
        return Err(SearchSkip::LowSaving);
    }
    Ok(SearchShape {
        text: out,
        matches,
        files: order.len(),
        omitted_lines: omitted,
        raw_tokens,
    })
}

/// Recognise a single-file shell read (`cat f`, `type f`, `Get-Content [-Raw] f`) and return the path.
pub fn shell_read_target(cmd: &str) -> Option<String> {
    let (program, args) = crate::command::single_invocation(cmd)?;
    let files: Vec<&String> = match program.as_str() {
        "cat" | "type" => args.iter().filter(|a| !a.starts_with('-')).collect(),
        "get-content" | "gc" => {
            let mut v = Vec::new();
            let mut skip = false;
            for a in &args {
                if skip {
                    skip = false;
                    continue;
                }
                let la = a.to_ascii_lowercase();
                if la == "-raw" || la == "-path" || la == "-literalpath" {
                    continue;
                }
                if la.starts_with('-') {
                    // -Encoding utf8, -TotalCount n, … change what's printed: don't shape
                    if la == "-encoding" {
                        skip = true;
                        continue;
                    }
                    return None;
                }
                v.push(a);
            }
            v
        }
        _ => return None,
    };
    if program == "cat" && args.iter().any(|a| a.starts_with('-')) {
        return None; // cat -n / -A change the output
    }
    match files.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    }
}

/// Recognise shell search commands whose stdout is grep-shaped.
pub fn is_shell_search(cmd: &str) -> bool {
    let fam = crate::command::family(cmd, false);
    matches!(
        fam.as_str(),
        "grep" | "rg" | "egrep" | "fgrep" | "git grep" | "select-string" | "findstr" | "ag" | "ack"
    )
}

/// Language-aware outline exists for this path (used to decide whether shell reads are worth shaping).
pub fn is_source_like(path: &str) -> bool {
    !matches!(outline::lang_for_path(path), Lang::Generic)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_cs(n_methods: usize) -> String {
        let mut s = String::from("namespace Big;\n\npublic class Service\n{\n");
        for i in 0..n_methods {
            s.push_str(&format!(
                "    public int Method{i}(int x)\n    {{\n        var y = x * {i};\n        return y + {i};\n    }}\n\n"
            ));
        }
        s.push_str("}\n");
        s
    }

    #[test]
    fn small_files_pass_through() {
        assert!(shape_read("a.cs", "class A {}\n", &ShapeConfig::default()).is_none());
    }

    #[test]
    fn large_file_head_is_verbatim_and_outline_has_ranges() {
        let src = big_cs(200);
        let cfg = ShapeConfig::default();
        let r = shape_read("Service.cs", &src, &cfg).expect("shaped");
        assert!(src.starts_with(&r.head));
        assert_eq!(r.head_lines, r.head.lines().count());
        assert!(r.head_lines <= cfg.head_lines);
        assert_eq!(r.total_lines, src.lines().count());
        assert!(
            r.outline.contains("3-1205  public class Service"),
            "{}",
            r.outline.chars().take(300).collect::<String>()
        );
        assert!(r.outline.contains("  5-9  public int Method0(int x)"));
        let ctx = read_context("Service.cs", &r, r.total_lines);
        assert!(ctx.contains("lines 1-") && ctx.contains("offset="));
        // Worst case for outlines (tiny 5-line methods): still at least a 50% saving.
        assert!(
            ctx.len() + r.head.len() < src.len() / 2,
            "meaningful saving"
        );
    }

    #[test]
    fn long_first_line_gives_empty_head() {
        let src = format!("{}\n", "x".repeat(10_000));
        let r = shape_read("min.js", &src, &ShapeConfig::default()).unwrap();
        assert_eq!(r.head_lines, 0);
        assert!(r.head.is_empty());
    }

    fn grep_output(files: usize, per_file: usize, prefix: &str) -> String {
        let mut s = String::new();
        for f in 0..files {
            for l in 0..per_file {
                s.push_str(&format!("{prefix}src{}Module{f}{}Handler.cs:{}:        var result = await _service.ProcessAsync(request, cancellationToken);\n", '/', '/', l * 7 + 1));
            }
        }
        s
    }

    #[test]
    fn search_groups_by_file_and_relativizes() {
        let cwd = "C:/work/repo";
        let text = grep_output(10, 8, "C:/work/repo/");
        let r = shape_search(&text, Some(cwd), &ShapeConfig::default(), Some("h_x"), None)
            .expect("shaped");
        assert_eq!(r.matches, 80);
        assert_eq!(r.files, 10);
        assert_eq!(r.omitted_lines, 0);
        assert!(
            r.text
                .starts_with("src/Module0/Handler.cs (8 matches)\n  1:        var result")
        );
        assert!(!r.text.contains("C:/work/repo/src"));
        assert!(
            !r.text.contains("aaes show"),
            "no handle when nothing omitted"
        );
        assert!(r.text.len() < text.len());
    }

    #[test]
    fn search_caps_per_file_with_exact_totals_and_handle() {
        let text = grep_output(2, 50, "");
        let r = shape_search(&text, None, &ShapeConfig::default(), Some("h_abc"), None).unwrap();
        assert_eq!(r.matches, 100);
        assert_eq!(r.omitted_lines, 60);
        assert!(r.text.contains("(50 matches, showing first 20 lines)"));
        assert!(r.text.contains("[aaes] 100 matches in 2 files"));
        assert!(r.text.contains("aaes show h_abc"));
    }

    #[test]
    fn windows_drive_paths_and_context_lines() {
        let mut text = String::new();
        for i in 0..60 {
            text.push_str(&format!(
                "C:\\repo\\src\\A.cs-{}-    // context line with some padding text here\n",
                i * 3
            ));
            text.push_str(&format!(
                "C:\\repo\\src\\A.cs:{}:    Foo(); // a match line with some padding text\n",
                i * 3 + 1
            ));
        }
        let r = shape_search(&text, Some("C:\\repo"), &ShapeConfig::default(), None, None).unwrap();
        assert_eq!(r.matches, 60);
        assert!(
            r.text
                .starts_with("src\\A.cs (60 matches, showing first 20 lines)\n  0-    // context")
        );
        assert!(r.text.contains("narrow the search"));
    }

    #[test]
    fn single_file_output_without_paths_is_capped_under_one_label() {
        // Grep tool on one file / `grep -n pat file`: `12:text` matches, `13-text` context.
        let text: String = (0..400)
            .map(|i| {
                if i % 4 == 3 {
                    format!("{i}-    context line around a match\n")
                } else {
                    format!("{i}:    some matching line content here\n")
                }
            })
            .collect();
        let r = shape_search(
            &text,
            None,
            &ShapeConfig::default(),
            Some("h_s"),
            Some("src/big.rs"),
        )
        .expect("shaped");
        assert_eq!((r.matches, r.files, r.omitted_lines), (300, 1, 380));
        assert!(
            r.text
                .starts_with("src/big.rs (300 matches, showing first 20 lines)\n  0:    some")
        );
        assert!(r.text.contains("`aaes show h_s`"));
        // Unknown file (shell grep): generic label.
        let r = shape_search(&text, None, &ShapeConfig::default(), Some("h_s"), None).unwrap();
        assert!(r.text.starts_with("(single file) (300 matches"));
        assert!(matches!(
            shape_search("a.rs:1:x\n", None, &ShapeConfig::default(), None, None),
            Err(SearchSkip::Small)
        ));
    }

    #[test]
    fn mixed_chained_output_is_shaped_with_other_lines_verbatim() {
        // `grep -rn … ; grep -n … one.cs`: 80% path-prefixed, 20% line-number-only
        let mut text = grep_output(4, 40, "src/");
        let extra: String = (0..40)
            .map(|i| format!("{i}:    single file hit {i}\n"))
            .collect();
        text.push_str(&extra);
        let r =
            shape_search(&text, None, &ShapeConfig::default(), Some("h_m"), None).expect("shaped");
        let tail = r
            .text
            .split("verbatim in original order:\n")
            .nth(1)
            .expect("other block");
        assert!(tail.starts_with("0:    single file hit 0\n1:    single file hit 1\n"));
        assert!(r.text.contains("[aaes] 40 other lines"));
        // Mostly line-number-only: grouped as one file, path lines kept verbatim.
        let mostly: String =
            (0..300).map(|i| format!("{i}: x\n")).collect::<String>() + &grep_output(1, 50, "a/");
        let r = shape_search(&mostly, None, &ShapeConfig::default(), Some("h_m"), None).unwrap();
        assert_eq!(r.files, 1);
        assert!(r.text.contains("[aaes] 50 other lines"));
        // Neither form reaches the bar: pass through.
        let neither: String = (0..300)
            .map(|i| format!("plain text line {i} without any position\n"))
            .collect::<String>()
            + &(0..300).map(|i| format!("{i}: x\n")).collect::<String>();
        assert_eq!(
            shape_search(&neither, None, &ShapeConfig::default(), None, None)
                .unwrap_err()
                .reason(),
            "unparsed"
        );
    }

    #[test]
    fn line_forms() {
        assert_eq!(line_form("12:  let x = 1;"), "lineno_only");
        assert_eq!(line_form("src/main.rs"), "path_only");
        assert_eq!(line_form("C:\\repo\\a.cs"), "path_only");
        assert_eq!(line_form("{\"type\":\"match\"}"), "json");
        assert_eq!(line_form("42"), "number");
        assert_eq!(line_form("Binary file x.bin matches"), "binary_notice");
        assert_eq!(line_form("hello world"), "other");
        // Multi-byte first character (a UTF-8 BOM from PowerShell output): no panic.
        assert_eq!(line_form("\u{feff}src/main.rs"), "path_only");
        assert_eq!(line_form("é/x.cs:3"), "colon_other");
        assert_eq!(line_form("日本/a.cs"), "path_only");
    }

    #[test]
    fn shell_read_detection() {
        assert_eq!(
            shell_read_target("cat src/a.cs").as_deref(),
            Some("src/a.cs")
        );
        assert_eq!(
            shell_read_target("Get-Content -Raw 'C:\\x\\a.cs'").as_deref(),
            Some("C:\\x\\a.cs")
        );
        assert_eq!(shell_read_target("Get-Content a.cs -TotalCount 20"), None);
        assert_eq!(shell_read_target("cat -n a.cs"), None);
        assert_eq!(shell_read_target("cat a.cs b.cs"), None);
        assert_eq!(shell_read_target("cat a.cs | head"), None);
        assert!(is_shell_search("rg -n Foo src"));
        assert!(is_shell_search("git grep -n Foo"));
        assert!(is_shell_search("Select-String -Path *.cs -Pattern Foo"));
        assert!(!is_shell_search("dotnet build"));
    }
}
