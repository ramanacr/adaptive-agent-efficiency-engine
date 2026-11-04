//! Shell command → *command family* (e.g. `cd src && dotnet test -c Release` → `dotnet test`).
//!
//! Families are the unit for prioritising parsers (ADR-001) and seeding the command registry (ADR-016).

/// Leading words skipped when looking for the real program.
const WRAPPERS: &[&str] = &["sudo", "time", "env", "nohup", "command", "exec", "rtk"];
/// Segments that only set up state; skipped when choosing the primary segment.
const SETUP: &[&str] = &[
    "cd",
    "pushd",
    "popd",
    "set",
    "export",
    "source",
    ".",
    "set-location",
    "sl",
];
/// Programs whose first non-flag argument is part of the family.
const MULTI: &[&str] = &[
    "git",
    "dotnet",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "ng",
    "nx",
    "npx",
    "docker",
    "kubectl",
    "helm",
    "cargo",
    "go",
    "gh",
    "az",
    "terraform",
    "pip",
    "uv",
    "poetry",
    "deno",
    "mvn",
    "gradle",
    "rtk",
];
/// Flags (per program) that consume the following token.
const FLAG_WITH_VALUE: &[(&str, &str)] = &[
    ("git", "-C"),
    ("git", "-c"),
    ("dotnet", "--project"),
    ("docker", "--context"),
    ("kubectl", "-n"),
    ("kubectl", "--namespace"),
    ("npm", "--prefix"),
    ("pnpm", "--filter"),
    ("pnpm", "-C"),
];

/// Well-known programs, kept verbatim even in `anonymize` mode.
const KNOWN: &[&str] = &[
    "git",
    "dotnet",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "ng",
    "nx",
    "npx",
    "node",
    "tsc",
    "eslint",
    "prettier",
    "jest",
    "vitest",
    "playwright",
    "docker",
    "kubectl",
    "helm",
    "cargo",
    "rustc",
    "go",
    "gh",
    "az",
    "terraform",
    "pip",
    "uv",
    "poetry",
    "python",
    "pytest",
    "deno",
    "mvn",
    "gradle",
    "ls",
    "dir",
    "cat",
    "type",
    "head",
    "tail",
    "grep",
    "rg",
    "find",
    "fd",
    "sed",
    "awk",
    "wc",
    "sort",
    "echo",
    "curl",
    "wget",
    "tree",
    "mkdir",
    "rm",
    "cp",
    "mv",
    "touch",
    "chmod",
    "diff",
    "jq",
    "make",
    "cmake",
    "msbuild",
    "nuget",
    "sqlcmd",
    "psql",
    "where",
    "which",
    "get-childitem",
    "get-content",
    "select-string",
    "test-path",
    "invoke-webrequest",
    "get-process",
    "write-output",
    "write-host",
];

/// Normalised command used for re-run detection: trimmed, whitespace collapsed.
pub fn normalize(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Map a shell command to its family. With `anonymize`, unknown programs become `other:<hash>`.
pub fn family(cmd: &str, anonymize: bool) -> String {
    let segments = split_segments(cmd);
    let primary = segments
        .iter()
        .find(|s| {
            first_program(s)
                .map(|(p, _)| !SETUP.contains(&p.as_str()))
                .unwrap_or(false)
        })
        .or_else(|| segments.first());
    let Some(seg) = primary else {
        return "(empty)".to_owned();
    };
    let Some((program, rest)) = first_program(seg) else {
        return "(empty)".to_owned();
    };
    let is_path = rest.is_path;
    let rest = rest.args;

    let mut fam = if is_path {
        format!("script:{program}")
    } else {
        program.clone()
    };

    if matches!(program.as_str(), "python" | "python3" | "py") {
        if let Some(i) = rest.iter().position(|a| a == "-m") {
            if let Some(m) = rest.get(i + 1) {
                return format!("python -m {}", m.to_ascii_lowercase());
            }
        }
    }

    if MULTI.contains(&program.as_str()) {
        if let Some(sub) = first_subcommand(&program, &rest) {
            fam = format!("{fam} {sub}");
        }
    }

    if anonymize && !KNOWN.contains(&program.as_str()) {
        let tail = fam
            .split_once(' ')
            .map(|(_, t)| format!(" {t}"))
            .unwrap_or_default();
        return format!("other:{:08x}{tail}", fnv1a(&program));
    }
    fam
}

/// For a command made of exactly one segment (no `&&`, `||`, `|`, `;`, newline or redirect), the
/// program (lower-cased, extension stripped) and its arguments with quotes removed.
pub fn single_invocation(cmd: &str) -> Option<(String, Vec<String>)> {
    if cmd.contains('>') || cmd.contains('<') {
        return None;
    }
    let segs = split_segments(cmd);
    if segs.len() != 1 {
        return None;
    }
    let (program, rest) = first_program(&segs[0])?;
    Some((program, rest.args))
}

struct Rest {
    args: Vec<String>,
    is_path: bool,
}

fn split_segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '\n' | ';' | '|' | '&' => {
                    // Treat `&&`, `||`, `|`, `;`, newline (and PowerShell `&` call operator) as separators.
                    if !cur.trim().is_empty() {
                        out.push(cur.trim().to_owned());
                    }
                    cur.clear();
                }
                _ => cur.push(c),
            },
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// Whitespace tokenizer that keeps quoted runs together and drops the quotes
/// (`"C:\Program Files\x.exe" build` → [`C:\Program Files\x.exe`, `build`]).
fn tokens(seg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in seg.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None if c == '(' || c == ')' => {}
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_env_assignment(t: &str) -> bool {
    match t.split_once('=') {
        Some((k, _)) => {
            !k.is_empty()
                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && k.chars().next().is_some_and(|c| !c.is_ascii_digit())
        }
        None => false,
    }
}

fn first_program(seg: &str) -> Option<(String, Rest)> {
    let toks = tokens(seg);
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        let lower = t.to_ascii_lowercase();
        if is_env_assignment(t) || (WRAPPERS.contains(&lower.as_str()) && i + 1 < toks.len()) {
            i += 1;
            continue;
        }
        break;
    }
    let raw = toks.get(i)?;
    let is_path = raw.contains('/') || raw.contains('\\');
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut program = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stripped) = program.strip_suffix(ext) {
            program = stripped.to_owned();
            break;
        }
    }
    // A path to a well-known tool (e.g. `C:\Program Files\dotnet\dotnet.exe`) is still that tool.
    let is_path = is_path && !KNOWN.contains(&program.as_str());
    Some((
        program,
        Rest {
            args: toks[i + 1..].to_vec(),
            is_path,
        },
    ))
}

fn first_subcommand(program: &str, args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with('-') {
            if FLAG_WITH_VALUE.iter().any(|(p, f)| *p == program && f == a) {
                i += 1;
            }
            i += 1;
            continue;
        }
        let looks_like_word = a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ':' || c == '_' || c == '.');
        return looks_like_word.then(|| a.to_ascii_lowercase());
    }
    None
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::family;

    fn f(c: &str) -> String {
        family(c, false)
    }

    #[test]
    fn basic_families() {
        assert_eq!(f("git status"), "git status");
        assert_eq!(f("git --no-pager -C src log --oneline -n 5"), "git log");
        assert_eq!(
            f("cd src/Api && dotnet test -c Release --no-build"),
            "dotnet test"
        );
        assert_eq!(f("npx ng test --watch=false"), "npx ng");
        assert_eq!(f("ng build --configuration production"), "ng build");
        assert_eq!(f("rg -n \"TODO\" src | head -20"), "rg");
        assert_eq!(f("FOO=1 BAR=2 npm run lint"), "npm run");
        assert_eq!(f("rtk git diff"), "git diff");
        assert_eq!(f("python -m pytest -q"), "python -m pytest");
    }

    #[test]
    fn windows_and_powershell() {
        assert_eq!(
            f(r#""C:\Program Files\dotnet\dotnet.exe" build"#),
            "dotnet build"
        );
        assert_eq!(
            f("Set-Location D:\\work; Get-ChildItem -Recurse"),
            "get-childitem"
        );
        assert_eq!(f(".\\build.ps1 -Target Test"), "script:build");
    }

    #[test]
    fn quoted_separators_do_not_split() {
        assert_eq!(f("git commit -m \"a && b\""), "git commit");
        assert_eq!(f("echo 'x | y'"), "echo");
    }

    #[test]
    fn anonymize_hides_unknown_programs_only() {
        assert_eq!(family("git status", true), "git status");
        let a = family("./scripts/deploy-internal.sh prod", true);
        assert!(a.starts_with("other:"), "{a}");
        assert!(!a.contains("deploy"));
    }

    #[test]
    fn single_invocation_only_for_one_segment() {
        use super::single_invocation;
        assert_eq!(
            single_invocation(r#"cat "src/My App/a.cs""#),
            Some(("cat".into(), vec!["src/My App/a.cs".into()]))
        );
        assert_eq!(
            single_invocation("Get-Content -Raw a.cs"),
            Some(("get-content".into(), vec!["-Raw".into(), "a.cs".into()]))
        );
        assert_eq!(single_invocation("cat a | head"), None);
        assert_eq!(single_invocation("cat a > b"), None);
        assert_eq!(single_invocation("cd x && cat a"), None);
    }

    #[test]
    fn setup_only_command_falls_back_to_first_segment() {
        assert_eq!(f("cd src"), "cd");
        assert_eq!(f(""), "(empty)");
    }
}
