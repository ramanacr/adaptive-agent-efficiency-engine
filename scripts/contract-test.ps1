#Requires -Version 7
<#
.SYNOPSIS
  Scripted contract test: AAES hooks against a real, headless Claude Code (FUN-014).

.DESCRIPTION
  Claude Code silently ignores a hook response whose shape it does not accept, so a
  Claude Code update can turn AAES into a no-op without any error. This script runs
  the M1 contract scenarios (docs/spikes/M1-contract-test.md) against the installed
  `claude` and checks, from AAES's own event log and the Claude Code transcript, that
  shaped output actually reached the model.

  AAES is isolated: a fresh AAES_HOME, a separate settings file and a separate server
  port. Your own AAES data and hooks are not touched.

  Two ways to authenticate Claude Code (-Auth):
    api           A fresh CLAUDE_CONFIG_DIR and an API key (see -ApiKeyEnv). Fully
                  isolated; billed to the API key.
    subscription  Your normal Claude Code login. Your user settings, plugins and MCP
                  servers are left out (--setting-sources project,local, --settings,
                  --strict-mcp-config), and nothing in ~/.claude is changed, but the
                  test sessions are saved under ~/.claude/projects and count against
                  your plan's usage.

  Hard checks (exit 1 on failure) depend on AAES and the hook contract only. Answers
  that depend on the model's instruction following are soft checks (warnings).

  Cost: about USD 0.30-0.50 per full run with the default model (haiku). In api mode
  the key goes only into the child process environment and is never printed.

.EXAMPLE
  pwsh scripts/contract-test.ps1 -Aaes target/release/aaes.exe -WorkDir C:\Claude_Builds\contract
#>
[CmdletBinding()]
param(
    # aaes binary under test. Default: `aaes` on PATH.
    [string]$Aaes = 'aaes',
    # Scratch folder for fixtures, isolated config and results. Default: a new temp folder.
    [string]$WorkDir = (Join-Path ([IO.Path]::GetTempPath()) "aaes-contract-$(Get-Date -Format yyyyMMdd-HHmmss)"),
    [string]$Model = 'haiku',
    [int]$Port = 47839,
    [string[]]$Scenarios = @('C1', 'C2', 'C3', 'C4', 'C5', 'C6'),
    [ValidateSet('api', 'subscription')]
    [string]$Auth = 'api',
    # User or process environment variable that holds the Anthropic API key.
    [string]$ApiKeyEnv = 'AAES_ANTHROPIC_API_KEY',
    # Fail (instead of warn) when `claude --version` differs from the pinned version.
    [switch]$Strict,
    # After a fully passing run, write the tested Claude Code version to the pin file.
    [switch]$UpdatePin
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$pinFile = Join-Path $repo 'tests/contract/claude-code.version'

function Resolve-Exe([string]$name) {
    $c = Get-Command $name -ErrorAction SilentlyContinue
    if (-not $c) { throw "not found: $name" }
    $c.Source
}

$Aaes = Resolve-Exe $Aaes
$claude = Resolve-Exe 'claude'
$key = $null
if ($Auth -eq 'api') {
    $key = [Environment]::GetEnvironmentVariable($ApiKeyEnv)
    if (-not $key) { $key = [Environment]::GetEnvironmentVariable($ApiKeyEnv, 'User') }
    if (-not $key) { throw "API key variable '$ApiKeyEnv' is not set (process or user scope)." }
}

# --- version pin -------------------------------------------------------------------
$ccVersion = ((& $claude --version) -split ' ')[0].Trim()
$pinned = if (Test-Path $pinFile) { (Get-Content $pinFile -Raw).Trim() } else { '' }
$versionNote = if ($ccVersion -eq $pinned) { 'pinned' } else { "differs from pin '$pinned'" }
Write-Host "Claude Code $ccVersion ($versionNote) · aaes $((& $Aaes --version) -join ' ') · model $Model"
if ($Strict -and $ccVersion -ne $pinned) { throw "Claude Code $ccVersion is not the pinned version $pinned (-Strict)." }

# --- isolated environment ----------------------------------------------------------
$ws = Join-Path $WorkDir 'ws'
$cc = Join-Path $WorkDir 'cc-config'
$ah = Join-Path $WorkDir 'aaes-home'
$out = Join-Path $WorkDir 'results'
foreach ($d in $ws, $cc, $ah, $out) { New-Item -ItemType Directory -Force $d | Out-Null }
# Read shaping is off by default since the M1 trial; the contract still covers it.
'{"read_shaping": true, "read_threshold_tokens": 2000}' | Set-Content -Encoding utf8NoBOM (Join-Path $ah 'config.json')

$settings = Join-Path $WorkDir 'settings.json'
$saved = @{}
foreach ($n in 'CLAUDE_CONFIG_DIR', 'AAES_HOME', 'ANTHROPIC_API_KEY', 'AAES_HOOK_TOKEN') { $saved[$n] = [Environment]::GetEnvironmentVariable($n) }
# A token inherited from the calling session would shadow the test server's token.
$env:AAES_HOOK_TOKEN = $null
$env:AAES_HOME = $ah
$claudeArgs = @('--model', $Model, '--output-format', 'json', '--max-turns', '20')
if ($Auth -eq 'api') {
    $env:CLAUDE_CONFIG_DIR = $cc
    $settings = Join-Path $cc 'settings.json'
    $transcripts = Join-Path $cc 'projects'
} else {
    $env:CLAUDE_CONFIG_DIR = $null
    $env:ANTHROPIC_API_KEY = $null
    $claudeArgs += '--setting-sources', 'project,local', '--settings', $settings, '--strict-mcp-config'
    $transcripts = Join-Path ($saved['CLAUDE_CONFIG_DIR'] ?? (Join-Path $HOME '.claude')) 'projects'
}

# --- fixtures ----------------------------------------------------------------------
function New-BigService([string]$path) {
    $sb = [Text.StringBuilder]::new()
    foreach ($l in 'using System;', '', 'namespace Contract.Big;', '', '/// Synthetic large service for the AAES contract test.', 'public class BigService', '{', '    private readonly int _offset = 7;', '') { [void]$sb.Append("$l`n") }
    for ($i = 0; $i -lt 300; $i++) {
        $name = if ($i -eq 123) { 'ComputeTotal123' } else { "Method$i" }
        [void]$sb.Append("    public int $name(int x)`n    {`n        var y = x * $i + _offset;`n        return y + $i;`n    }`n`n")
    }
    [void]$sb.Append("}`n")
    [IO.File]::WriteAllText($path, $sb.ToString())
}

function Reset-Workspace {
    Get-ChildItem $ws -Force | Remove-Item -Recurse -Force
    New-BigService (Join-Path $ws 'BigService.cs')
    for ($f = 1; $f -le 8; $f++) {
        $dir = New-Item -ItemType Directory -Force (Join-Path $ws "src/Feature$f")
        $lines = @("namespace Contract.Feature$f;", '', 'public class Handler', '{', '    public async Task Run(Request request, CancellationToken cancellationToken)', '    {')
        for ($k = 1; $k -le 30; $k++) { $lines += "        await _mediator.Send(new Command$k(request.Id), cancellationToken);" }
        $lines += '    }', '}'
        [IO.File]::WriteAllText((Join-Path $dir 'Handler.cs'), ($lines -join "`n") + "`n")
    }
    for ($f = 1; $f -le 15; $f++) {
        $n = '{0:D2}' -f $f
        [IO.File]::WriteAllText((Join-Path $ws "f$n.txt"), ((1..20 | ForEach-Object { "file $n" }) -join "`n") + "`n")
    }
}

# --- scenarios ---------------------------------------------------------------------
$prompts = @{
    C1 = 'Automated test. Use the Read tool to read BigService.cs (whole file, no offset or limit). Then, using only what you received and without any other tool calls, reply with ONLY a JSON object {"first_line": "<first line of the file, verbatim>", "compute_total_123_lines": "<start>-<end>"} giving the line range of the method ComputeTotal123.'
    C2 = "Automated test. Read BigService.cs with the Read tool (whole file). Then use the Edit tool to change the method Method250 so that it returns 0: replace its line 'return y + 250;' with 'return 0;'. Your Edit old_string must match the file exactly. Reply with ONLY the word SUCCESS, or the exact error message if the edit failed."
    C3 = "Automated test. Use the Grep tool once with pattern 'Send\(' , path src, output_mode content and -n true. Then reply with ONLY a JSON object {`"matches`": <total number of matching lines>, `"files`": <number of files with matches>}, based on the tool output."
    C4 = "Automated test. Use the Agent tool to launch a general-purpose subagent with this exact task: 'Read BigService.cs with the Read tool (whole file, no offset or limit) and, using only what that Read returned, report the line range of the method ComputeTotal123 as start-end.' Then reply with ONLY the subagent's answer."
    C5 = 'Automated test. Use the Read tool on each of these files, one call per file: f01.txt f02.txt f03.txt f04.txt f05.txt f06.txt f07.txt f08.txt f09.txt f10.txt f11.txt f12.txt f13.txt f14.txt f15.txt. Issue them sequentially. Then reply DONE.'
    C6 = "Automated test. Use the Bash tool once to run exactly this command (backticks not included): ``grep -rn 'Send(' src`` and then reply with ONLY a JSON object {`"matches`": <total number of matching lines>, `"files`": <number of files with matches>}, based on the tool output."
}
$tools = @{ C1 = 'Read'; C2 = 'Read,Edit'; C3 = 'Grep'; C4 = 'Read,Agent,Task'; C5 = 'Read'; C6 = 'Bash(grep:*)' }

function Invoke-Claude([string]$id) {
    Reset-Workspace
    if ($key) { $env:ANTHROPIC_API_KEY = $key }
    Push-Location $ws
    try {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $raw = & $claude -p $prompts[$id] @claudeArgs --allowedTools $tools[$id] 2> (Join-Path $out "$id.err")
        $code = $LASTEXITCODE
        $ms = $sw.ElapsedMilliseconds
    } finally {
        Pop-Location
        if ($key) { $env:ANTHROPIC_API_KEY = $null }
    }
    $raw | Set-Content -Encoding utf8NoBOM (Join-Path $out "$id.json")
    $j = try { ($raw -join "`n") | ConvertFrom-Json } catch { $null }
    $sid = $j.session_id
    $events = @()
    if ($sid) {
        $ef = Join-Path $ah "sessions/$sid/events.jsonl"
        if (Test-Path $ef) { $events = @(Get-Content $ef | ForEach-Object { $_ | ConvertFrom-Json }) }
    }
    $transcript = ''
    if ($sid) {
        $main = Get-ChildItem $transcripts -Recurse -Filter "$sid.jsonl" -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($main) {
            $files = @($main.FullName) + @(Get-ChildItem (Join-Path $main.DirectoryName $sid) -Recurse -Filter *.jsonl -ErrorAction SilentlyContinue | ForEach-Object FullName)
            $transcript = ($files | ForEach-Object { Get-Content $_ -Raw }) -join "`n"
        }
    }
    [pscustomobject]@{
        id = $id; exit = $code; ms = $ms; session = $sid; answer = [string]$j.result
        cost = [double]($j.total_cost_usd ?? 0); events = $events; transcript = $transcript
    }
}

function Check($r, [string]$name, [bool]$ok, [switch]$Soft) {
    $r.checks += [pscustomobject]@{ name = $name; ok = $ok; soft = [bool]$Soft }
}

function Get-Json([string]$text) {
    $m = [regex]::Match($text, '\{[^{}]*\}')
    if ($m.Success) { try { return $m.Value | ConvertFrom-Json } catch {} }
    $null
}

$results = @()
try {
    # Always name the settings file: without it, init would edit the user's own settings.
    $initOut = & $Aaes init --mode http --port $Port --settings $settings 2>&1
    if ($LASTEXITCODE -ne 0) { throw "aaes init failed: $initOut" }

    foreach ($id in $Scenarios) {
        Write-Host "running $id ..."
        $r = Invoke-Claude $id
        $r | Add-Member checks @()
        Check $r 'claude exit 0' ($r.exit -eq 0)
        Check $r 'session events recorded' ($r.events.Count -gt 0)
        $shapedReads = @($r.events | Where-Object { $_.kind -eq 'read' -and $_.action -eq 'shaped' })
        switch ($id) {
            'C1' {
                Check $r 'read shaped in main context' (@($shapedReads | Where-Object ctx -eq 'main').Count -ge 1)
                Check $r 'outline delivered (hook_additional_context)' ($r.transcript -match '"type":"hook_additional_context"' -and $r.transcript -match '\[aaes\] Large file')
                $a = Get-Json $r.answer
                Check $r 'first line from verbatim head' ($a.first_line -eq 'using System;')
                Check $r 'range from outline (748-752)' ($a.compute_total_123_lines -eq '748-752') -Soft
            }
            'C2' {
                $text = [IO.File]::ReadAllLines((Join-Path $ws 'BigService.cs'))
                Check $r 'read shaped' ($shapedReads.Count -ge 1)
                Check $r 'edit applied (line 1513 is return 0;)' ($text[1512] -eq '        return 0;')
                Check $r 'answer SUCCESS' ($r.answer.Trim() -match '^SUCCESS') -Soft
            }
            'C3' {
                $s = @($r.events | Where-Object { $_.kind -eq 'search' -and $_.action -eq 'shaped' -and $_.tool -eq 'Grep' })
                Check $r 'Grep result shaped' ($s.Count -ge 1)
                Check $r 'shaped text reached transcript (aaes show handle)' ($r.transcript -match 'aaes show h_')
                $a = Get-Json $r.answer
                Check $r 'exact totals (240 matches, 8 files)' ($a.matches -eq 240 -and $a.files -eq 8)
            }
            'C4' {
                Check $r 'read shaped inside sub-agent' (@($shapedReads | Where-Object ctx -ne 'main').Count -ge 1)
                Check $r 'sub-agent answer 748-752' ($r.answer -match '748\s*-\s*752') -Soft
            }
            'C5' {
                $p = @($r.events | Where-Object { $_.kind -eq 'read' -and $_.action -eq 'pass' })
                Check $r '15 small reads passed through' ($p.Count -ge 15 -and $shapedReads.Count -eq 0)
                $d = @($r.events | ForEach-Object { [int64]$_.dur_us } | Sort-Object)
                if ($d.Count) { $r | Add-Member hook_p50_us $d[[int][math]::Floor(($d.Count - 1) * 0.5)] }
            }
            'C6' {
                $s = @($r.events | Where-Object { $_.kind -eq 'search' -and $_.action -eq 'shaped' -and $_.tool -eq 'Bash' })
                Check $r 'shell grep result shaped' ($s.Count -ge 1)
                $a = Get-Json $r.answer
                Check $r 'exact totals (240 matches, 8 files)' ($a.matches -eq 240 -and $a.files -eq 8)
            }
        }
        $results += $r
    }
} finally {
    & $Aaes serve --stop --port $Port 2>&1 | Out-Null
    foreach ($n in $saved.Keys) { [Environment]::SetEnvironmentVariable($n, $saved[$n]) }
}

# Schema drift and hook errors show up in AAES's own log (fail open, so nothing else would).
$env:AAES_HOME = $ah
$logErrors = @(& $Aaes logs --errors 2>&1 | Where-Object { $_ -match '\S' -and $_ -notmatch 'no (log|errors)' })
$drift = @(Get-ChildItem (Join-Path $ah 'logs') -Filter *.jsonl -ErrorAction SilentlyContinue | Select-String 'unexpected tool payload shape')
$env:AAES_HOME = $saved['AAES_HOME']

# --- report ------------------------------------------------------------------------
$hardFail = 0; $softFail = 0
$md = @("# AAES contract test — $(Get-Date -Format 'yyyy-MM-dd HH:mm')", '',
    "Claude Code **$ccVersion** ($versionNote) · auth $Auth · model ``$Model`` · aaes ``$((& $Aaes --version) -join ' ')``", '',
    '| Scenario | Check | Result |', '|---|---|---|')
foreach ($r in $results) {
    foreach ($c in $r.checks) {
        $res = if ($c.ok) { 'pass' } elseif ($c.soft) { $softFail++; 'WARN' } else { $hardFail++; 'FAIL' }
        $md += "| $($r.id) | $($c.name) | $res |"
    }
}
$md += "| all | no schema-drift warnings | $(if ($drift.Count) { $hardFail++; 'FAIL' } else { 'pass' }) |"
$md += "| all | no AAES log errors | $(if ($logErrors.Count) { $hardFail++; 'FAIL' } else { 'pass' }) |"
$cost = ($results | Measure-Object cost -Sum).Sum
$p50 = ($results | Where-Object { $_.PSObject.Properties['hook_p50_us'] } | Select-Object -First 1).hook_p50_us
$tail = "Hard failures: **$hardFail** · warnings: $softFail · cost (as reported by Claude Code; plan usage in subscription mode): USD $([math]::Round($cost, 3))"
if ($p50) { $tail += " · hook p50 (C5, server side): $p50 µs" }
$md += '', $tail
if ($logErrors.Count) { $md += '', 'AAES log errors:', '```', ($logErrors | Select-Object -First 20), '```' }
$md | Set-Content -Encoding utf8NoBOM (Join-Path $out 'summary.md')
$md -join "`n" | Write-Host
Write-Host "`nDetails: $out"

if ($hardFail -eq 0 -and $UpdatePin -and $ccVersion -ne $pinned) {
    New-Item -ItemType Directory -Force (Split-Path $pinFile) | Out-Null
    $ccVersion | Set-Content -Encoding utf8NoBOM $pinFile
    Write-Host "Pinned Claude Code $ccVersion in $pinFile"
}
exit ([int]($hardFail -gt 0))
