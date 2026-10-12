# Cold restart — Vite + Savvy

Both must be restarted together, in this order. Vite is **not** optional.

## Why

`src-tauri/tauri.conf.json` wires the frontend two ways:

```json
"devUrl": "http://localhost:5183",
"frontendDist": "../ui/build",
"beforeDevCommand": "npm run dev"
```

The debug binary expects Vite on **5183** (`ui/vite.config.ts`: `port: 5183, strictPort: true`).
If Vite is not running, the app falls back to `../ui/build` — and that directory is
stale build output, **not** the source you just edited. Edits to `ui/src/**` silently
take no effect, which is worse than a visible error because everything *looks* fine.

Never trust a behavioural change until `:5183` is listening first.

## Order of operations

1. Kill `savvy-app` and **only the Vite node processes** — a leftover Vite from a
   previous session is a trap (it may be serving an older tree), but killing *every*
   `node` is worse: the tool driving this session is itself a node process, and
   `Get-Process node | Stop-Process` takes it down mid-command. Filter on the child
   command line instead (see the script).
2. Start Vite on 5183 and wait until it actually **listens** (it takes ~1s, but the
   `npm run dev` wrapper makes it look slower).
3. Only then start `savvy-app.exe`.
4. Verify with the check below.

## The script

```powershell
$root = "D:\Nextcloud\[02] Projekte\[04] Coding\Savvy"; $t = "$env:TEMP\opencode"

Get-Process savvy-app -ErrorAction SilentlyContinue | Stop-Process -Force
# Never `Get-Process node | Stop-Process` — match only Vite's own child processes.
Get-CimInstance Win32_Process -Filter "Name='node.exe'" |
    Where-Object { $_.CommandLine -like "*Savvy\ui\node_modules*" } |
    ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 2
Remove-Item -LiteralPath "$t\tauri-out.log","$t\tauri-err.log","$t\vite.log" -Force -ErrorAction SilentlyContinue

# NOTE: do NOT use Start-Process -WorkingDirectory here (see pitfalls below).
$ui = Join-Path $root "ui"
Start-Process -FilePath "cmd.exe" -ArgumentList "/c","cd /d `"$ui`" && npm run dev > `"$t\vite.log`" 2>&1"

$ok = $false
for ($i=0; $i -lt 60; $i++) {
    Start-Sleep -Seconds 1
    if (Get-NetTCPConnection -LocalPort 5183 -State Listen -ErrorAction SilentlyContinue) { $ok = $true; break }
}
if (-not $ok) { Write-Error "vite never came up on :5183"; return }
"vite listening (after ${i}s)"

$bin = Join-Path $root "target\debug\savvy-app.exe"
Start-Process -FilePath $bin -WorkingDirectory $t -WindowStyle Hidden `
    -RedirectStandardOutput "$t\tauri-out.log" -RedirectStandardError "$t\tauri-err.log"
Start-Sleep -Seconds 12
```

`-WorkingDirectory $t` on the *app* is fine — the temp path has no brackets. The
problem is only with the project path.

## Verify

```powershell
Get-Content -LiteralPath "$env:TEMP\opencode\tauri-err.log" -Tail 6; "---"; Get-Process savvy-app -ErrorAction SilentlyContinue | Select-Object Id,StartTime | Format-Table -AutoSize
```

Expect a clean `tauri-err.log` (empty output, no traceback), then a `savvy-app` row
with a fresh `StartTime`. Confirm Vite too:

```powershell
Get-NetTCPConnection -LocalPort 5183 -State Listen -ErrorAction SilentlyContinue
```

## Pitfalls — the path-with-brackets traps

The repo lives at `D:\Nextcloud\[02] Projekte\[04] Coding\Savvy`. The `[02]` and
`[04]` are **literal directory names**, but PowerShell treats `[]` as wildcard
characters in every `-Path`-style parameter. `-LiteralPath` disables that.

| Wrong | Right |
|---|---|
| `Start-Process -WorkingDirectory "$root\ui"` | `Start-Process cmd.exe -ArgumentList "/c","cd /d `"$root\ui`" && …"` |
| `Get-Content -Path "...\SPEC-v1.md"` | `Get-Content -LiteralPath "...\SPEC-v1.md"` |
| `Select-String -Path "...\+page.svelte"` | `Select-String -LiteralPath "...\+page.svelte"` |
| `Get-ChildItem -Path "$root\ui\src"` (ok only if no brackets in what it matches) | prefer `-LiteralPath` |

### `Start-Process -WorkingDirectory`

Fails with:

> Der Vorgang kann nicht ausgeführt werden, weil der aufgelöste Platzhalterpfad
> "D:\Nextcloud\[02] Projekte\[04] Coding\Savvy\ui" keine Datei angibt.
> (`FileOpenFailure`, `PathNotFound`)

`Start-Process` has **no `-LiteralPath` for `-WorkingDirectory`**, so there is no way
to hand it the path directly. Two workarounds:

1. **`cmd /c cd /d "<path>" && <command>`** — recommended. `cmd.exe` treats `[` and
   `]` literally. This is what the Vite line above does.
2. `Set-Location -LiteralPath <path>` then start the process with no
   `-WorkingDirectory`, relying on the caller's CWD.

### `cmd /c ... && ...` handed straight to PowerShell

Running `cmd.exe /c cd /d "<path>" && cargo test` *as written* never reaches `cmd`:
PowerShell 5.1 parses the whole line first and rejects `&&`:

> Das Token "&&" ist in dieser Version kein gültiges Anweisungstrennzeichen.
> (`InvalidEndOfLine`)

The whole shell pipeline has to be one quoted argument, with embedded quotes doubled:

```powershell
cmd.exe /c "cd /d ""D:\...\src-tauri"" && cargo test --lib"
```

Anything longer than a one-liner belongs in a `.ps1` executed via
`powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File <path>` — inline
multi-line commands can fail to spawn at all (`spawn EPERM`).

### `Get-Content` / `Select-String` / `Get-ChildItem`

`-LiteralPath` is the safe default throughout this repo. Using `-Path` (or the bare
path argument) silently glob-expands — a bracketed path usually matches *nothing*,
which surfaces as a confusing "path not found" for a file that demonstrably exists.

### Encoding — the silent corruption

`Get-Content` **without `-Encoding`** decodes a BOM-less UTF-8 file as the ANSI
codepage (CP1252 here). Round-tripping it through `Set-Content -Encoding UTF8`
bakes the mojibake in — `—` becomes `â€”`, `─` becomes `â”€` — and `Set-Content`
*also adds a BOM*. Every later edit then reads correctly (the BOM now says UTF-8),
so only the first pass is visibly wrong and the damage looks original.

Either use the edit tool, or keep the whole read/write cycle in .NET where the
encoding is explicit:

```powershell
$s  = [System.IO.File]::ReadAllText($p, [System.Text.Encoding]::UTF8)
[System.IO.File]::WriteAllText($p, $s, (New-Object System.Text.UTF8Encoding($false)))
```

If a file is already corrupted, the CP1252 round-trip reverses it: encode the
mojibake string with CP1252 to recover the original bytes, then decode as UTF-8.
Prove it is lossless first (`$cp.GetString($cp.GetBytes($s)) -eq $s`), and strip
the BOM on the way out.

### Concatenation instead of `Join-Path`

`$root + "\ui"` works; but if the result is ever passed to a `-Path` parameter the
same wildcard problem returns. Prefer `Join-Path $root "ui"` for building, and
`-LiteralPath` for consuming.

## Reading logs during a run

```powershell
$t = "$env:TEMP\opencode"
Get-Content -LiteralPath "$t\tauri-out.log" -Tail 40 | ForEach-Object { $_ -replace '\x1b\[[0-9;]*m','' }
```

The ANSI colour codes are stripped, otherwise `Select-String` output is unreadable.
Useful filters: `"proxy upstream"`, `"resolved"`, `"innertube"`.
