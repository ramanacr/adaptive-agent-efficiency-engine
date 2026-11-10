"""Generate the synthetic golden fixtures for the read/search fidelity tests (M1 exit).

Run from the repository root:  python tests/fixtures/gen_golden.py
Output: tests/fixtures/search/*.txt + *.json, tests/fixtures/read/*.
All content is synthetic (ADR-020). Expected shaped outputs (*.expected) are written by
`AAES_UPDATE_GOLDEN=1 cargo test -p aaes-core --test golden` and reviewed by hand.
"""
import json
import os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)))
SEARCH = os.path.join(ROOT, "search")
READ = os.path.join(ROOT, "read")

PAD = "await _service.ProcessAsync(request, cancellationToken); // synthetic padding"


def write(path, text, newline="\n"):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text.replace("\n", newline))


def meta(name, **kw):
    base = {"cwd": None, "single_path": None, "expect": "shaped"}
    base.update(kw)
    with open(os.path.join(SEARCH, name + ".json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(base, f, indent=2)
        f.write("\n")


def search_fixtures():
    os.makedirs(SEARCH, exist_ok=True)

    # 1. GNU grep -rn, relative Unix paths, uneven per-file counts (some files capped).
    counts = [3, 25, 40, 1, 12, 60]
    out = ""
    for f, n in enumerate(counts):
        for i in range(n):
            out += f"src/Module{f}/Handler.cs:{i * 5 + 1}:        {PAD} {f}.{i}\n"
    write(os.path.join(SEARCH, "gnu_grep_rn.txt"), out)
    meta("gnu_grep_rn", matches=sum(counts), files=len(counts))

    # 2. Absolute paths under the working folder: relativized.
    out = ""
    for f in range(5):
        for i in range(15):
            out += f"/home/dev/repo/lib/part{f}/mod.rs:{i * 3 + 2}:    let value = compute_{f}_{i}(input); // {PAD}\n"
    write(os.path.join(SEARCH, "abs_paths_cwd.txt"), out)
    meta("abs_paths_cwd", cwd="/home/dev/repo", matches=75, files=5)

    # 3. rg -n -C1: context lines `path-N-text`, group separators `--`.
    out = ""
    matches = 0
    for f in range(4):
        for i in range(12):
            base = i * 10 + 5
            out += f"web/app/comp{f}.ts-{base - 1}-  // before the call {PAD}\n"
            out += f"web/app/comp{f}.ts:{base}:  this.store.dispatch(load{f}x{i}()); {PAD}\n"
            out += f"web/app/comp{f}.ts-{base + 1}-  // after the call {PAD}\n"
            out += "--\n"
            matches += 1
    write(os.path.join(SEARCH, "rg_context.txt"), out)
    meta("rg_context", matches=matches, files=4)

    # 4. git grep -n with a binary-file notice (kept verbatim as an other line).
    out = ""
    for f in range(3):
        for i in range(30):
            out += f"docs/guide{f}.md:{i + 1}:See the configuration section for option {i}. {PAD}\n"
    out += "Binary file assets/logo.png matches\n"
    write(os.path.join(SEARCH, "git_grep_binary.txt"), out)
    meta("git_grep_binary", matches=90, files=3)

    # 5. PowerShell Select-String on Windows: drive paths, CRLF line endings.
    out = ""
    for f in range(4):
        for i in range(22):
            out += f"C:\\repo\\src\\Feature{f}\\Service.cs:{i * 4 + 1}:    _logger.LogInformation(\"step {i}\"); // {PAD}\n"
    write(os.path.join(SEARCH, "select_string_crlf.txt"), out, newline="\r\n")
    meta("select_string_crlf", cwd="C:\\repo", matches=88, files=4)

    # 6. Grep tool on one file: `N:text` matches and `N-text` context, labelled by the caller.
    out = ""
    m = 0
    for i in range(120):
        if i % 4 == 3:
            out += f"{i + 1}-    // context around a match {PAD}\n"
        else:
            out += f"{i + 1}:    matchingCall({i}); {PAD}\n"
            m += 1
    write(os.path.join(SEARCH, "single_file_lineno.txt"), out)
    meta("single_file_lineno", single_path="src/big.rs", matches=m, files=1)

    # 7. Chained commands: mostly path lines plus single-file `N:text` lines (kept verbatim).
    out = ""
    for f in range(4):
        for i in range(30):
            out += f"src/api/ctrl{f}.py:{i + 1}:    return handler_{f}_{i}(request)  # {PAD}\n"
    for i in range(20):
        out += f"{i + 1}:    extra single file hit {i} {PAD}\n"
    write(os.path.join(SEARCH, "mixed_chained.txt"), out)
    meta("mixed_chained", matches=120, files=4)

    # 8. Output truncated by the host in the middle of a line (no final newline).
    out = ""
    for f in range(3):
        for i in range(35):
            out += f"pkg/store{f}/db.go:{i * 2 + 1}:\tif err := tx.Commit(); err != nil {{ // {PAD}\n"
    out += "pkg/store3/db.go:7:\tif err := tx.Comm"
    write(os.path.join(SEARCH, "truncated.txt"), out)
    meta("truncated", matches=106, files=4)

    # 9. UTF-8 BOM on the first line, non-Latin paths and text.
    out = "\ufeff"
    for f in range(3):
        for i in range(25):
            out += f"src/日本{f}/ファイル.ts:{i + 1}:  const 名前{i} = 'значение {i}'; // {PAD}\n"
    write(os.path.join(SEARCH, "utf8_bom.txt"), out)
    meta("utf8_bom", matches=75, files=3)

    # 10. Not grep output: passes through as unparsed.
    out = "".join(f"plain build log line {i} without any file position {PAD}\n" for i in range(200))
    write(os.path.join(SEARCH, "unparsed_plain.txt"), out)
    meta("unparsed_plain", expect="unparsed")

    # 11. Below the size threshold: passes through as small.
    out = "".join(f"src/a{i}.rs:{i}:x\n" for i in range(5))
    write(os.path.join(SEARCH, "small.txt"), out)
    meta("small", expect="small")

    # 12. Grep tool on Windows: paths under the working folder relativized, one outside kept.
    out = ""
    for path, n in [("C:\\repo\\src\\App\\Program.cs", 30), ("C:\\repo\\src\\Lib\\Mediator.cs", 9),
                    ("D:\\other\\Shared.cs", 4)]:
        for i in range(n):
            out += f"{path}:{i + 1}:        services.AddScoped<IHandler, Handler{i}>(); // {PAD}\n"
    write(os.path.join(SEARCH, "grep_tool_outside_cwd.txt"), out)
    meta("grep_tool_outside_cwd", cwd="C:\\repo", matches=43, files=3)

    # 13. Many files with one short match each: grouping saves too little, passes through.
    out = "".join(f"src/m{i}.rs:1:x\n" for i in range(600))
    write(os.path.join(SEARCH, "one_match_per_file.txt"), out)
    meta("one_match_per_file", expect="low_saving")


def read_fixtures():
    os.makedirs(READ, exist_ok=True)
    s = "namespace Golden.Services;\n\npublic class OrderService\n{\n"
    for i in range(120):
        s += f"    public decimal Total{i}(Order order)\n    {{\n        var sum = order.Lines.Sum(l => l.Price * {i});\n        return sum + {i};\n    }}\n\n"
    s += "}\n"
    write(os.path.join(READ, "OrderService.cs"), s)

    t = "import { Component } from '@angular/core';\n\n@Component({ selector: 'app-grid', templateUrl: './grid.html' })\nexport class GridComponent {\n"
    for i in range(150):
        t += f"  column{i}(row: Row): string {{\n    return row.cells[{i}].text.trim();\n  }}\n\n"
    t += "}\n"
    write(os.path.join(READ, "grid.component.ts"), t)

    md = "# Golden guide\n\n"
    for i in range(60):
        md += f"## Section {i}\n\n" + "".join(f"Paragraph {i}.{j} explains a synthetic topic in plain words.\n" for j in range(6)) + "\n"
    write(os.path.join(READ, "guide.md"), md)


if __name__ == "__main__":
    search_fixtures()
    read_fixtures()
    print("fixtures written")
