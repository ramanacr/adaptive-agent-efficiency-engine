//! Deterministic, line-oriented structural outlines (ADR-022).
//!
//! No parser libraries: these extractors are fast, dependency-free and good enough to navigate a
//! file. Every item has an exact, 1-based, inclusive line range that the agent can pass to
//! `Read(offset, limit)`. Accuracy is best-effort; exactness lives in the ranges, which always
//! point at real lines.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// 1-based first line.
    pub start: usize,
    /// 1-based last line (inclusive).
    pub end: usize,
    /// Nesting level for display.
    pub depth: usize,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// C#, Java, Kotlin, Go, Rust, C/C++, Swift, PHP, Dart.
    CLike,
    /// TypeScript / JavaScript.
    Script,
    Css,
    Html,
    Json,
    Yaml,
    Markdown,
    Generic,
}

pub fn lang_for_path(path: &str) -> Lang {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "cs" | "java" | "kt" | "kts" | "go" | "rs" | "c" | "h" | "cc" | "cpp" | "hpp" | "cxx"
        | "swift" | "php" | "dart" | "scala" | "groovy" => Lang::CLike,
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" => Lang::Script,
        "css" | "scss" | "less" | "sass" => Lang::Css,
        "html" | "htm" | "cshtml" | "razor" | "vue" | "svelte" | "xml" | "xaml" | "csproj"
        | "props" | "targets" | "config" => Lang::Html,
        "json" | "jsonc" => Lang::Json,
        "yml" | "yaml" => Lang::Yaml,
        "md" | "markdown" | "mdx" => Lang::Markdown,
        _ => Lang::Generic,
    }
}

pub fn outline(lang: Lang, text: &str) -> Vec<Item> {
    let lines: Vec<&str> = text.lines().collect();
    let items = match lang {
        Lang::CLike => brace_outline(&lines, false, 3, classify_clike),
        Lang::Script => brace_outline(&lines, false, 3, classify_script),
        Lang::Css => brace_outline(&lines, false, 2, classify_css),
        Lang::Json => brace_outline(&lines, true, 2, classify_json),
        Lang::Html => html_outline(&lines),
        Lang::Yaml => yaml_outline(&lines),
        Lang::Markdown => markdown_outline(&lines),
        Lang::Generic => Vec::new(),
    };
    if items.is_empty() {
        generic_outline(&lines)
    } else {
        items
    }
}

/// Render items as an indented list, shrinking depth until it fits the limits.
/// Returns the text and the number of items left out.
pub fn render(items: &[Item], max_items: usize, max_bytes: usize) -> (String, usize) {
    let full = render_with(items, max_items, max_bytes, false);
    if full.1 == 0 {
        return full;
    }
    // Too big: compact member labels (`ComputeTotal(int x)` instead of `public int ComputeTotal(int x)`)
    // so that large files still get a complete outline.
    let compact = render_with(items, max_items, max_bytes, true);
    if compact.1 < full.1 { compact } else { full }
}

/// `public async Task<Invoice> GetAsync(int id)` → `GetAsync(int id)`; other labels unchanged.
pub fn compact_label(label: &str) -> String {
    let Some(p) = label.find('(') else {
        return label.to_owned();
    };
    let before = &label[..p];
    let start = before
        .rfind(|c: char| c.is_whitespace() || c == '>')
        .map_or(0, |i| i + 1);
    let name = &before[start..];
    if name.is_empty() {
        return label.to_owned();
    }
    let rest = &label[p..];
    let close = rest.find(')').map_or(rest.len(), |i| i + 1);
    let params = &rest[..close];
    if params.chars().count() > 40 {
        let cut: String = params.chars().take(37).collect();
        format!("{name}{cut}…)")
    } else {
        format!("{name}{params}")
    }
}

fn render_with(
    items: &[Item],
    max_items: usize,
    max_bytes: usize,
    compact: bool,
) -> (String, usize) {
    let line_of = |it: &Item| {
        let range = if it.start == it.end {
            format!("{}", it.start)
        } else {
            format!("{}-{}", it.start, it.end)
        };
        if compact && it.depth >= 1 {
            format!(
                "{}{range} {}\n",
                "  ".repeat(it.depth),
                compact_label(&it.label)
            )
        } else {
            format!("{}{range}  {}\n", "  ".repeat(it.depth), it.label)
        }
    };
    let lines: Vec<String> = items.iter().map(line_of).collect();
    let max_depth = items.iter().map(|i| i.depth).max().unwrap_or(0);

    // Deepest level whose items (with all shallower ones) fit entirely.
    let fits = |limit: usize| {
        let (n, b) = items
            .iter()
            .zip(&lines)
            .filter(|(i, _)| i.depth <= limit)
            .fold((0, 0), |(n, b), (_, l)| (n + 1, b + l.len()));
        n <= max_items && b <= max_bytes
    };
    let full = (0..=max_depth).rev().find(|&d| fits(d));
    let mut keep: Vec<bool> = match full {
        Some(d) => items.iter().map(|i| i.depth <= d).collect(),
        None => vec![false; items.len()],
    };
    // Fill the remaining budget with the next level, in document order.
    let next = full.map_or(0, |d| d + 1);
    let mut n = keep.iter().filter(|k| **k).count();
    let mut bytes: usize = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.len())
        .sum();
    for (i, it) in items.iter().enumerate() {
        if !keep[i] && it.depth == next && n < max_items && bytes + lines[i].len() <= max_bytes {
            keep[i] = true;
            n += 1;
            bytes += lines[i].len();
        }
    }
    let out: String = lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.as_str())
        .collect();
    (out, items.len() - n)
}

// ---------------------------------------------------------------------------------------------
// Brace languages
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Container whose direct children are members (types, namespaces, JSON objects…).
    Container,
    Member,
}

#[derive(Default)]
struct Scan {
    in_block_comment: bool,
    in_template: bool,
}

/// Count structural opens/closes on a line, ignoring strings and comments.
fn scan_line(line: &str, st: &mut Scan, brackets: bool) -> (i32, i32) {
    let b = line.as_bytes();
    let (mut opens, mut closes) = (0, 0);
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        let c = b[i];
        if st.in_block_comment {
            if c == b'*' && b.get(i + 1) == Some(&b'/') {
                st.in_block_comment = false;
                i += 1;
            }
        } else if st.in_template {
            if c == b'\\' {
                i += 1;
            } else if c == b'`' {
                st.in_template = false;
            }
        } else if let Some(q) = quote {
            if c == b'\\' {
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else {
            match c {
                b'/' if b.get(i + 1) == Some(&b'/') => break,
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    st.in_block_comment = true;
                    i += 1;
                }
                b'"' => quote = Some(c),
                b'\'' if !brackets => quote = Some(c),
                b'`' if !brackets => st.in_template = true,
                b'{' => opens += 1,
                b'}' => closes += 1,
                b'[' if brackets => opens += 1,
                b']' if brackets => closes += 1,
                _ => {}
            }
        }
        i += 1;
    }
    (opens, closes)
}

struct Ctx {
    depth: usize,
    in_container_body: bool,
}

fn label(t: &str) -> String {
    let t = t.trim_end_matches('{').trim_end();
    if t.chars().count() > 110 {
        let s: String = t.chars().take(107).collect();
        format!("{s}…")
    } else {
        t.to_owned()
    }
}

fn brace_outline(
    lines: &[&str],
    brackets: bool,
    max_depth: usize,
    classify: fn(&str, &Ctx) -> Option<Kind>,
) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    // (item index, depth before the item's line, kind)
    let mut stack: Vec<(usize, usize, Kind)> = Vec::new();
    // depths that are the *body* of an open container
    let mut bodies: Vec<usize> = Vec::new();
    let mut pending: Option<(usize, usize, Kind)> = None;
    let mut st = Scan::default();
    let mut depth = 0usize;

    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        let in_code_at_start = !st.in_block_comment && !st.in_template;
        let d = depth;
        let (opens, closes) = scan_line(raw, &mut st, brackets);

        if in_code_at_start && !t.is_empty() {
            let ctx = Ctx {
                depth: d,
                in_container_body: bodies.last() == Some(&d) || (d == 0 && bodies.is_empty()),
            };
            let kind = if d <= max_depth {
                classify(t, &ctx)
            } else {
                None
            };
            if let Some(kind) = kind {
                pending = None;
                items.push(Item {
                    start: i + 1,
                    end: i + 1,
                    depth: stack.len(),
                    label: label(t),
                });
                let idx = items.len() - 1;
                if opens > closes {
                    stack.push((idx, d, kind));
                    if kind == Kind::Container {
                        bodies.push(d + 1);
                    }
                } else if opens == 0 && !t.ends_with(';') && !t.ends_with(',') && !t.ends_with('}')
                {
                    pending = Some((idx, d, kind)); // brace may follow on the next line (Allman)
                }
            } else if let Some((pidx, pd, pk)) = pending {
                if t.starts_with('{') && d == pd && opens > closes {
                    stack.push((pidx, pd, pk));
                    if pk == Kind::Container {
                        bodies.push(pd + 1);
                    }
                    pending = None;
                } else if !(t.starts_with("//")
                    || t.starts_with('[')
                    || t.starts_with(':')
                    || t.starts_with("where "))
                {
                    pending = None;
                }
            }
        }

        depth = (d as i32 + opens - closes).max(0) as usize;
        while let Some(&(idx, bd, kind)) = stack.last() {
            if depth <= bd {
                items[idx].end = i + 1;
                stack.pop();
                if kind == Kind::Container {
                    bodies.pop();
                }
            } else {
                break;
            }
        }
    }
    let last = lines.len().max(1);
    for (idx, _, _) in stack {
        items[idx].end = last;
    }
    items
}

fn first_word(t: &str) -> &str {
    t.split(|c: char| c.is_whitespace() || c == '(' || c == '<' || c == ':')
        .next()
        .unwrap_or("")
}

fn has_word(t: &str, words: &[&str]) -> bool {
    t.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .take(8)
        .any(|w| words.contains(&w))
}

const CONTROL: &[&str] = &[
    "if",
    "else",
    "for",
    "foreach",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "try",
    "catch",
    "finally",
    "return",
    "throw",
    "await",
    "var",
    "let",
    "const",
    "using",
    "new",
    "yield",
    "lock",
    "break",
    "continue",
    "goto",
    "base",
    "this",
    "get",
    "set",
    "init",
    "add",
    "remove",
    "when",
    "select",
    "from",
    "where",
    "fixed",
    "checked",
    "unchecked",
    "import",
    "package",
    "super",
    "delete",
    "typeof",
    "elif",
    "match",
    "loop",
    "defer",
    "go",
    "assert",
    "echo",
];

const CLIKE_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "override",
    "virtual",
    "abstract",
    "async",
    "sealed",
    "readonly",
    "partial",
    "extern",
    "unsafe",
    "event",
    "implicit",
    "explicit",
    "final",
    "synchronized",
    "open",
    "suspend",
    "inline",
    "pub",
    "operator",
    "required",
];
const CLIKE_TYPES: &[&str] = &[
    "class",
    "interface",
    "record",
    "struct",
    "enum",
    "trait",
    "impl",
    "object",
    "protocol",
    "extension",
    "union",
    "mod",
    "namespace",
];

fn looks_like_fn_signature(t: &str) -> bool {
    match t.find('(') {
        Some(p) => {
            let before = &t[..p];
            !before.contains('=') && !before.contains('.') && !before.trim().is_empty()
        }
        None => false,
    }
}

fn classify_clike(t: &str, ctx: &Ctx) -> Option<Kind> {
    if t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with('#')
        || t.starts_with('[')
        || t.starts_with('@')
        || t.starts_with('}')
        || t.starts_with('{')
    {
        return None;
    }
    let w = first_word(t);
    if CONTROL.contains(&w) && w != "default" {
        return None;
    }
    if has_word(t, CLIKE_TYPES) && !t.contains("=>") && !t.ends_with(';') {
        return Some(Kind::Container);
    }
    if t.starts_with("namespace ") {
        return Some(Kind::Container); // file-scoped `namespace X;`
    }
    if !ctx.in_container_body {
        // top-level functions (Go/Rust/Kotlin/C)
        if has_word(t, &["fn", "func", "fun"]) {
            return Some(Kind::Member);
        }
        return None;
    }
    let assign_before_paren = matches!((t.find('='), t.find('(')), (Some(e), Some(p)) if e < p);
    let is_field = t.ends_with(';') && (!t.contains('(') || assign_before_paren);
    if is_field && !t.contains("=>") {
        return None;
    }
    if has_word(t, &["fn", "func", "fun"]) || CLIKE_MODIFIERS.contains(&w) {
        return Some(Kind::Member);
    }
    if looks_like_fn_signature(t) || (t.contains('{') && t.contains("get")) {
        return Some(Kind::Member);
    }
    None
}

const SCRIPT_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "static",
    "readonly",
    "async",
    "get",
    "set",
    "override",
    "abstract",
    "declare",
    "accessor",
    "constructor",
];
const ANGULAR_DECORATORS: &[&str] = &[
    "@Component(",
    "@Injectable(",
    "@NgModule(",
    "@Directive(",
    "@Pipe(",
];
const ANGULAR_MEMBER_DECORATORS: &[&str] = &[
    "@Input(",
    "@Output(",
    "@ViewChild(",
    "@ViewChildren(",
    "@ContentChild(",
    "@HostListener(",
    "@HostBinding(",
];

fn classify_script(t: &str, ctx: &Ctx) -> Option<Kind> {
    if t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with('}')
        || t.starts_with('{')
        || t.starts_with("import ")
    {
        return None;
    }
    if ctx.depth == 0 {
        if ANGULAR_DECORATORS.iter().any(|d| t.starts_with(d)) {
            return Some(Kind::Member);
        }
        let t2 = t
            .trim_start_matches("export ")
            .trim_start_matches("default ")
            .trim_start_matches("declare ")
            .trim_start_matches("abstract ");
        if t2.starts_with("class ")
            || t2.starts_with("interface ")
            || t2.starts_with("enum ")
            || t2.starts_with("namespace ")
            || t2.starts_with("module ")
        {
            return Some(Kind::Container);
        }
        if t2.starts_with("function ")
            || t2.starts_with("async function ")
            || t2.starts_with("type ")
            || t2.starts_with("const ")
            || t2.starts_with("let ")
            || t2.starts_with("var ")
        {
            return Some(Kind::Member);
        }
        return None;
    }
    if !ctx.in_container_body {
        return None;
    }
    if ANGULAR_MEMBER_DECORATORS.iter().any(|d| t.starts_with(d)) {
        return Some(Kind::Member);
    }
    let w = first_word(t);
    if CONTROL.contains(&w) && w != "get" && w != "set" {
        return None;
    }
    if SCRIPT_MODIFIERS.contains(&w) {
        return Some(Kind::Member);
    }
    // method `name(…) {` / `name<T>(…): T {` or arrow property `name = (…) =>`
    let ident_end = t
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#'))
        .unwrap_or(t.len());
    if ident_end > 0 {
        let rest = t[ident_end..].trim_start();
        if rest.starts_with('(') || rest.starts_with('<') {
            return if t.ends_with(';') {
                None
            } else {
                Some(Kind::Member)
            };
        }
        if rest.starts_with('=') && t.contains("=>") {
            return Some(Kind::Member);
        }
    }
    None
}

fn classify_css(t: &str, _ctx: &Ctx) -> Option<Kind> {
    if t.starts_with('}') || t.starts_with("//") || t.starts_with("/*") {
        return None;
    }
    if t.ends_with('{')
        || (t.starts_with('@')
            && (t.contains("mixin") || t.contains("media") || t.contains("function")))
    {
        return Some(Kind::Container);
    }
    None
}

fn classify_json(t: &str, ctx: &Ctx) -> Option<Kind> {
    if ctx.depth >= 1 && ctx.depth <= 2 && t.starts_with('"') && t.contains("\":") {
        return Some(Kind::Container);
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Indentation / markup languages
// ---------------------------------------------------------------------------------------------

fn close_by_next_sibling(items: &mut [Item], keys: &[usize], last_line: usize) {
    for k in 0..items.len() {
        let mut end = last_line;
        for j in k + 1..items.len() {
            if keys[j] <= keys[k] {
                end = items[j].start - 1;
                break;
            }
        }
        items[k].end = end.max(items[k].start);
    }
}

fn last_nonblank(lines: &[&str]) -> usize {
    lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(1)
}

fn markdown_outline(lines: &[&str]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut levels = Vec::new();
    let mut fence = false;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence || !t.starts_with('#') {
            continue;
        }
        let level = t.chars().take_while(|&c| c == '#').count();
        if level > 6 || !t[level..].starts_with(' ') {
            continue;
        }
        items.push(Item {
            start: i + 1,
            end: i + 1,
            depth: level - 1,
            label: label(t),
        });
        levels.push(level);
    }
    close_by_next_sibling(&mut items, &levels, last_nonblank(lines));
    items
}

fn yaml_outline(lines: &[&str]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut indents = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let indent = l.len() - l.trim_start().len();
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with('-') || indent > 2 {
            continue;
        }
        if let Some(p) = t.find(':') {
            let key = &t[..p];
            if !key.is_empty() && !key.contains(' ')
                || key.starts_with('"')
                || key.starts_with('\'')
            {
                items.push(Item {
                    start: i + 1,
                    end: i + 1,
                    depth: indent / 2,
                    label: label(t),
                });
                indents.push(indent);
            }
        }
    }
    close_by_next_sibling(&mut items, &indents, last_nonblank(lines));
    items
}

const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr", "param", "keygen",
];
const STRUCT_TAGS: &[&str] = &[
    "html",
    "head",
    "body",
    "header",
    "nav",
    "main",
    "section",
    "article",
    "aside",
    "footer",
    "form",
    "table",
    "thead",
    "tbody",
    "ul",
    "ol",
    "dl",
    "ng-template",
    "ng-container",
    "template",
    "dialog",
    "details",
    "fieldset",
    "Project",
    "ItemGroup",
    "PropertyGroup",
    "Target",
    "configuration",
];

/// Net element depth change on a line: `<tag` opens, `</tag` and `/>` close.
fn html_delta(line: &str) -> i32 {
    let b = line.as_bytes();
    let mut d = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if b.get(i + 1) == Some(&b'/') {
                d -= 1;
            } else if b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
                let name: String = line[i + 1..]
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == ':' || *c == '.')
                    .collect();
                // find the end of this tag on the same line
                let close = line[i..].find('>').map(|p| i + p);
                let self_closing = close.is_some_and(|p| p > 0 && b[p - 1] == b'/');
                if !VOID_TAGS.contains(&name.to_ascii_lowercase().as_str()) && !self_closing {
                    d += 1;
                }
            }
        }
        i += 1;
    }
    d
}

fn html_outline(lines: &[&str]) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    let mut stack: Vec<(usize, i32)> = Vec::new();
    let mut depth: i32 = 0;
    let mut in_comment = false;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if in_comment {
            if t.contains("-->") {
                in_comment = false;
            }
            continue;
        }
        if t.starts_with("<!--") {
            in_comment = !t.contains("-->");
            continue;
        }
        let d = depth;
        let delta = html_delta(t);
        let is_flow = t.starts_with("@if")
            || t.starts_with("@for")
            || t.starts_with("@switch")
            || t.starts_with("@defer")
            || t.starts_with("@else");
        if t.starts_with('<') && !t.starts_with("</") && !t.starts_with("<!") && d <= 4 {
            let name: String = t[1..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == ':')
                .collect();
            let interesting = STRUCT_TAGS.contains(&name.as_str())
                || name.contains('-')
                || t.contains(" id=")
                || t.contains(" #")
                || t.contains("*ngIf")
                || t.contains("*ngFor")
                || t.contains("[formGroup]")
                || t.contains("(ngSubmit)");
            if interesting && d <= 3 {
                items.push(Item {
                    start: i + 1,
                    end: i + 1,
                    depth: stack.len(),
                    label: label(t),
                });
                if delta > 0 {
                    stack.push((items.len() - 1, d));
                }
            }
        } else if is_flow && d <= 4 {
            // Angular control-flow block: ends at the matching brace.
            let mut bal = 0i32;
            let mut end = i + 1;
            for (j, l2) in lines.iter().enumerate().skip(i) {
                let s = l2.replace("{{", "").replace("}}", "");
                bal += s.matches('{').count() as i32 - s.matches('}').count() as i32;
                if bal <= 0 && j > i || (j == i && bal == 0 && s.contains('}')) {
                    end = j + 1;
                    break;
                }
                end = j + 1;
            }
            items.push(Item {
                start: i + 1,
                end,
                depth: stack.len(),
                label: label(t),
            });
        }
        depth = (d + delta).max(0);
        while let Some(&(idx, bd)) = stack.last() {
            if depth <= bd {
                items[idx].end = i + 1;
                stack.pop();
            } else {
                break;
            }
        }
    }
    let last = lines.len().max(1);
    for (idx, _) in stack {
        items[idx].end = last;
    }
    items
}

/// Fallback: blank-line separated blocks, labelled by their first line.
fn generic_outline(lines: &[&str]) -> Vec<Item> {
    let mut items = Vec::new();
    let mut start: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.trim().is_empty() {
            if let Some(s) = start.take() {
                items.push(Item {
                    start: s + 1,
                    end: i,
                    depth: 0,
                    label: label(lines[s].trim()),
                });
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        items.push(Item {
            start: s + 1,
            end: lines.len(),
            depth: 0,
            label: label(lines[s].trim()),
        });
    }
    // Very long unstructured text (logs): fixed-size chunks instead of one giant block.
    if items.len() <= 1 && lines.len() > 200 {
        items.clear();
        let step = lines.len().div_ceil(40).max(50);
        let mut s = 0;
        while s < lines.len() {
            let e = (s + step).min(lines.len());
            items.push(Item {
                start: s + 1,
                end: e,
                depth: 0,
                label: label(lines[s].trim()),
            });
            s = e;
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[Item]) -> Vec<(usize, usize, usize, String)> {
        items
            .iter()
            .map(|i| (i.start, i.end, i.depth, i.label.clone()))
            .collect()
    }

    #[test]
    fn csharp_allman_and_file_scoped_namespace() {
        let src = "using System;\n\nnamespace Contoso.Billing;\n\n[Serializable]\npublic class InvoiceService : IInvoiceService\n{\n    private readonly ILogger _log;\n\n    public InvoiceService(ILogger log)\n    {\n        _log = log;\n    }\n\n    public async Task<Invoice> GetAsync(int id)\n    {\n        if (id < 0)\n        {\n            throw new ArgumentException();\n        }\n        return await Load(id);\n    }\n\n    public int Count { get; set; }\n\n    private static string Format(Invoice i) => i.ToString();\n}\n";
        let it = outline(Lang::CLike, src);
        assert_eq!(
            labels(&it),
            vec![
                (3, 3, 0, "namespace Contoso.Billing;".into()),
                (
                    6,
                    27,
                    0,
                    "public class InvoiceService : IInvoiceService".into()
                ),
                (10, 13, 1, "public InvoiceService(ILogger log)".into()),
                (
                    15,
                    22,
                    1,
                    "public async Task<Invoice> GetAsync(int id)".into()
                ),
                (24, 24, 1, "public int Count { get; set; }".into()),
                (
                    26,
                    26,
                    1,
                    "private static string Format(Invoice i) => i.ToString();".into()
                ),
            ]
        );
    }

    #[test]
    fn csharp_block_namespace_nesting() {
        let src = "namespace A {\n  public interface IFoo {\n    void Run(int x);\n  }\n  internal class Foo : IFoo {\n    public void Run(int x) { }\n  }\n}\n";
        let it = outline(Lang::CLike, src);
        let l = labels(&it);
        assert_eq!(l[0], (1, 8, 0, "namespace A".into()));
        assert_eq!(l[1], (2, 4, 1, "public interface IFoo".into()));
        assert_eq!(l[2], (3, 3, 2, "void Run(int x);".into()));
        assert_eq!(l[3], (5, 7, 1, "internal class Foo : IFoo".into()));
        assert_eq!(l[4], (6, 6, 2, "public void Run(int x) { }".into()));
    }

    #[test]
    fn angular_component() {
        let src = "import { Component, Input } from '@angular/core';\n\n@Component({\n  selector: 'app-invoice',\n  templateUrl: './invoice.component.html',\n})\nexport class InvoiceComponent implements OnInit {\n  @Input() invoiceId!: number;\n  private total = 0;\n  constructor(private api: ApiService) {}\n\n  ngOnInit(): void {\n    const x = this.api.load(this.invoiceId);\n    if (x) {\n      this.total = 1;\n    }\n  }\n\n  onSave = (e: Event) => {\n    this.api.save();\n  };\n}\n\nexport function helper(a: number): number {\n  return a * 2;\n}\n";
        let it = outline(Lang::Script, src);
        let l = labels(&it);
        assert_eq!(l[0], (3, 6, 0, "@Component(".into()));
        assert_eq!(
            l[1],
            (
                7,
                22,
                0,
                "export class InvoiceComponent implements OnInit".into()
            )
        );
        assert_eq!(l[2], (8, 8, 1, "@Input() invoiceId!: number;".into()));
        assert_eq!(l[3], (9, 9, 1, "private total = 0;".into()));
        assert_eq!(
            l[4],
            (10, 10, 1, "constructor(private api: ApiService) {}".into())
        );
        assert_eq!(l[5], (12, 17, 1, "ngOnInit(): void".into()));
        assert_eq!(l[6], (19, 21, 1, "onSave = (e: Event) =>".into()));
        assert_eq!(
            l[7],
            (
                24,
                26,
                0,
                "export function helper(a: number): number".into()
            )
        );
        assert_eq!(l.len(), 8, "locals and if-blocks are not listed");
    }

    #[test]
    fn scss_json_yaml_markdown() {
        let scss = ".card {\n  padding: 4px;\n  .title {\n    color: red;\n  }\n}\n@media (max-width: 600px) {\n  .card { padding: 0; }\n}\n";
        let l = labels(&outline(Lang::Css, scss));
        assert_eq!(l[0], (1, 6, 0, ".card".into()));
        assert_eq!(l[1], (3, 5, 1, ".title".into()));
        assert_eq!(l[2].0, 7);

        let json = "{\n  \"name\": \"app\",\n  \"scripts\": {\n    \"build\": \"ng build\",\n    \"test\": \"ng test\"\n  },\n  \"deps\": [\n    \"a\"\n  ]\n}\n";
        let l = labels(&outline(Lang::Json, json));
        assert_eq!(l[0], (2, 2, 0, "\"name\": \"app\",".into()));
        assert_eq!(
            l[1],
            (
                3,
                6,
                0,
                "\"scripts\": {".trim_end_matches('{').trim_end().into()
            )
        );
        assert_eq!(l[2], (4, 4, 1, "\"build\": \"ng build\",".into()));
        assert_eq!(l[4].0, 7);
        assert_eq!(l[4].1, 9);

        let yaml =
            "name: ci\non:\n  push:\n    branches: [main]\njobs:\n  build:\n    runs-on: ubuntu\n";
        let l = labels(&outline(Lang::Yaml, yaml));
        assert_eq!(l[0], (1, 1, 0, "name: ci".into()));
        assert_eq!(l[1], (2, 4, 0, "on:".into()));
        assert_eq!(l[2], (3, 4, 1, "push:".into()));
        assert_eq!(l[3], (5, 7, 0, "jobs:".into()));

        let md = "# Title\ntext\n## A\n```\n# not a heading\n```\n## B\nmore\n";
        let l = labels(&outline(Lang::Markdown, md));
        assert_eq!(
            l,
            vec![
                (1, 8, 0, "# Title".into()),
                (3, 6, 1, "## A".into()),
                (7, 8, 1, "## B".into())
            ]
        );
    }

    #[test]
    fn html_angular_template() {
        let html = "<div class=\"page\">\n  <app-header></app-header>\n  <form [formGroup]=\"form\" (ngSubmit)=\"save()\">\n    <input formControlName=\"name\">\n    @if (error) {\n      <p>{{ error }}</p>\n    }\n  </form>\n</div>\n";
        let l = labels(&outline(Lang::Html, html));
        assert_eq!(l[0], (2, 2, 0, "<app-header></app-header>".into()));
        assert_eq!(
            l[1],
            (
                3,
                8,
                0,
                "<form [formGroup]=\"form\" (ngSubmit)=\"save()\">".into()
            )
        );
        assert_eq!(l[2], (5, 7, 1, "@if (error)".into()));
    }

    #[test]
    fn generic_and_render_limits() {
        let text = "alpha\nbeta\n\ngamma\n";
        assert_eq!(
            labels(&outline(Lang::Generic, text)),
            vec![(1, 2, 0, "alpha".into()), (4, 4, 0, "gamma".into())]
        );
        let items: Vec<Item> = (0..10)
            .map(|i| Item {
                start: i + 1,
                end: i + 1,
                depth: i % 2,
                label: format!("item {i}"),
            })
            .collect();
        // 5 items at depth 0 fit; the remaining budget (none) takes no depth-1 items.
        let (txt, omitted) = render(&items, 5, 10_000);
        assert_eq!(txt.lines().count(), 5);
        assert!(
            txt.lines().all(|l| !l.starts_with("  ")),
            "deepest level dropped first:\n{txt}"
        );
        assert_eq!(omitted, 5);
        // With room for 7, two depth-1 items are added in document order.
        let (txt, omitted) = render(&items, 7, 10_000);
        assert_eq!(txt.lines().count(), 7);
        assert_eq!(txt.lines().filter(|l| l.starts_with("  ")).count(), 2);
        assert!(txt.contains("  2  item 1") && txt.contains("  4  item 3"));
        assert_eq!(omitted, 3);
    }

    #[test]
    fn compact_labels() {
        assert_eq!(
            compact_label("public async Task<Invoice> GetAsync(int id)"),
            "GetAsync(int id)"
        );
        assert_eq!(compact_label("ngOnInit(): void"), "ngOnInit()");
        assert_eq!(
            compact_label("public int Count { get; set; }"),
            "public int Count { get; set; }"
        );
        assert_eq!(
            compact_label("onSave = (e: Event) =>"),
            "onSave = (e: Event) =>"
        );
        assert!(
            compact_label("void F(string aaaaaaaaaa, string bbbbbbbbbb, string cccccccccc)")
                .ends_with("…)")
        );
    }

    #[test]
    fn large_class_gets_complete_compact_outline() {
        let mut src = String::from("public class Big\n{\n");
        for i in 0..300 {
            src.push_str(&format!(
                "    public int Method{i}(int x)\n    {{\n        return x + {i};\n    }}\n"
            ));
        }
        src.push_str("}\n");
        let items = outline(Lang::CLike, &src);
        assert_eq!(items.len(), 301);
        let (txt, omitted) = render(&items, 400, 10_000);
        assert_eq!(omitted, 0, "all 300 methods listed");
        assert!(
            txt.contains("\n  1199-1202 Method299(int x)\n"),
            "{}",
            &txt[txt.len() - 120..]
        );
    }

    #[test]
    fn lang_detection() {
        assert_eq!(lang_for_path(r"C:\src\App\Program.cs"), Lang::CLike);
        assert_eq!(lang_for_path("src/app/a.component.ts"), Lang::Script);
        assert_eq!(lang_for_path("styles.scss"), Lang::Css);
        assert_eq!(lang_for_path("app.component.html"), Lang::Html);
        assert_eq!(lang_for_path("package.json"), Lang::Json);
        assert_eq!(lang_for_path("README"), Lang::Generic);
    }
}
