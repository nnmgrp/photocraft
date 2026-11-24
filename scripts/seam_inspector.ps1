<#
.SYNOPSIS
    PhotoCraft: Manga Suite — Upstream Seam Inspector.
    Scans all changes outside the isolated "suitcase" crates
    (crates/manga-core, crates/manga-ui, crates/manga-bridge, crates/tone, TODO, scripts/seam_inspector.ps1)
    relative to the upstream PhotoCraft merge-base, and generates a file-by-file,
    line-by-line Markdown report (default: TODO/UPSTREAM_SEAMS.md).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts/seam_inspector.ps1
    powershell -ExecutionPolicy Bypass -File scripts/seam_inspector.ps1 -BaseRef upstream/main -OutFile TODO/UPSTREAM_SEAMS.md
#>
param(
    [string]$BaseRef = "",
    [string]$OutFile = "TODO/UPSTREAM_SEAMS.md",
    [switch]$IncludePatch
)

$ErrorActionPreference = "Stop"

$repoRoot = (git rev-parse --show-toplevel).Trim()
Push-Location $repoRoot
try {
    if ([string]::IsNullOrWhiteSpace($BaseRef)) {
        $detected = $null
        try {
            $detected = (git merge-base HEAD upstream/main 2>$null).Trim()
        } catch {}
        if ([string]::IsNullOrWhiteSpace($detected)) {
            $detected = "5bdbcc1181bb17134430fb927ec6945e7d3c6a31"
        }
        $baseSha = $detected
    } else {
        $baseSha = (git merge-base HEAD $BaseRef).Trim()
    }

    $headSha = (git rev-parse --short HEAD).Trim()
    $headBranch = (git rev-parse --abbrev-ref HEAD).Trim()
    $baseShort = (git rev-parse --short $baseSha).Trim()
    $baseSubject = (git log -1 --format="%s" $baseSha).Trim()

    function Is-SuitcasePath([string]$p) {
        return (
            $p -match '^crates/(manga-core|manga-ui|manga-bridge|tone)/' -or
            $p -match '^TODO/' -or
            $p -eq 'scripts/seam_inspector.ps1'
        )
    }

    function Get-GroupKey([string]$p) {
        if ($p -match '^(crates/[^/]+)/') { return $Matches[1] }
        if ($p -match '^(apps/[^/]+)/') { return $Matches[1] }
        if ($p -match '^(\.github)/') { return ".github" }
        if ($p -match '^(assets)/') { return "assets" }
        return "(root)"
    }

    # Collect numstat and name-status
    $statusMap = @{}
    git diff --name-status "$baseSha..HEAD" | ForEach-Object {
        if ($_ -match '^([A-Z])[0-9]*\t(.+)$') {
            $st = $Matches[1]
            $path = $Matches[2]
            if (-not (Is-SuitcasePath $path)) {
                $statusMap[$path] = $st
            }
        }
    }

    $statsMap = @{}
    git diff --numstat "$baseSha..HEAD" | ForEach-Object {
        $parts = $_ -split "`t"
        if ($parts.Length -ge 3) {
            $path = $parts[2]
            if ($statusMap.ContainsKey($path)) {
                $statsMap[$path] = [PSCustomObject]@{
                    Added   = $parts[0]
                    Removed = $parts[1]
                }
            }
        }
    }

    $modifiedFiles = @($statusMap.Keys | Where-Object { $statusMap[$_] -eq 'M' } | Sort-Object)
    $addedFiles    = @($statusMap.Keys | Where-Object { $statusMap[$_] -eq 'A' } | Sort-Object)
    $otherFiles    = @($statusMap.Keys | Where-Object { $statusMap[$_] -notin @('M', 'A') } | Sort-Object)

    $totalModAdded = 0
    $totalModRemoved = 0
    $totalHunks = 0
    $fileHunks = @{}

    foreach ($file in $modifiedFiles) {
        $st = $statsMap[$file]
        if ($st.Added -match '^\d+$')   { $totalModAdded   += [int]$st.Added }
        if ($st.Removed -match '^\d+$') { $totalModRemoved += [int]$st.Removed }
        $fileHunks[$file] = New-Object System.Collections.Generic.List[object]
    }

    # Single-pass git diff -U0 (fast on Windows)
    $modSet = New-Object 'System.Collections.Generic.HashSet[string]'
    foreach ($f in $modifiedFiles) {
        if ($f -ne 'Cargo.lock') { [void]$modSet.Add($f) }
    }

    $activeFile = $null
    $current = $null
    git diff -U0 --no-renames "$baseSha..HEAD" | ForEach-Object {
        $line = $_
        if ($line -match '^diff --git a/(.+) b/(.+)$') {
            if ($null -ne $current -and $null -ne $activeFile) {
                $fileHunks[$activeFile].Add($current)
                $totalHunks++
                $current = $null
            }
            $cand = $Matches[2]
            $activeFile = if ($modSet.Contains($cand)) { $cand } else { $null }
            return
        }
        if ($null -eq $activeFile) { return }

        if ($line -match '^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$') {
            if ($null -ne $current) {
                $fileHunks[$activeFile].Add($current)
                $totalHunks++
            }
            $oldCount = if (-not [string]::IsNullOrEmpty($Matches[2])) { [int]$Matches[2] } else { 1 }
            $newStart = [int]$Matches[3]
            $newCount = if (-not [string]::IsNullOrEmpty($Matches[4])) { [int]$Matches[4] } else { 1 }
            $ctx = $Matches[5].Trim()

            $rangeLabel = if ($newCount -eq 0) {
                "L$newStart (del -$oldCount)"
            } elseif ($newCount -eq 1) {
                "L$newStart"
            } else {
                "L$newStart-L$($newStart + $newCount - 1)"
            }

            $current = [PSCustomObject]@{
                NewStart   = $newStart
                NewCount   = $newCount
                OldCount   = $oldCount
                RangeLabel = $rangeLabel
                Context    = $ctx
                Preview    = ""
                PatchLines = (New-Object System.Collections.Generic.List[string])
            }
        } elseif ($null -ne $current) {
            if ($line.StartsWith('+') -and -not $line.StartsWith('+++')) {
                $trimmed = $line.Substring(1).Trim()
                if ([string]::IsNullOrEmpty($current.Preview) -and -not [string]::IsNullOrEmpty($trimmed)) {
                    $current.Preview = "+ " + $trimmed
                }
                if ($IncludePatch) { $current.PatchLines.Add($line) }
            } elseif ($line.StartsWith('-') -and -not $line.StartsWith('---')) {
                $trimmed = $line.Substring(1).Trim()
                if ([string]::IsNullOrEmpty($current.Preview) -and -not [string]::IsNullOrEmpty($trimmed)) {
                    $current.Preview = "- " + $trimmed
                }
                if ($IncludePatch) { $current.PatchLines.Add($line) }
            }
        }
    }
    if ($null -ne $current -and $null -ne $activeFile) {
        $fileHunks[$activeFile].Add($current)
        $totalHunks++
    }

    $sb = New-Object System.Text.StringBuilder
    [void]$sb.AppendLine("# Upstream Seam Report (Правки вне «чемодана»)")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("Автоматически сгенерировано инспектором ``scripts/seam_inspector.ps1``.")
    [void]$sb.AppendLine("Показывает **все** изменения вне изолированных крейтов чемодана (``crates/manga-core``, ``crates/manga-ui``, ``crates/manga-bridge``, ``crates/tone``) относительно базового коммита апстрима PhotoCraft — пофайлово и построчно, чтобы при обновлении апстрима или поломке сразу видеть каждый шов.")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("## Сводка")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("| Параметр | Значение |")
    [void]$sb.AppendLine("|---|---|")
    [void]$sb.AppendLine("| Ветка / HEAD | ``$headBranch`` (``$headSha``) |")
    [void]$sb.AppendLine("| База апстрима (merge-base) | ``$baseShort`` — $baseSubject |")
    [void]$sb.AppendLine("| Изменено файлов апстрима (``M``) | **$($modifiedFiles.Count)** (``+$totalModAdded / -$totalModRemoved`` строк, **$totalHunks** швов/hunk-ов) |")
    [void]$sb.AppendLine("| Добавлено новых файлов вне чемодана (``A``) | **$($addedFiles.Count)** |")
    [void]$sb.AppendLine("| Как обновить отчёт | ``powershell -ExecutionPolicy Bypass -File scripts/seam_inspector.ps1`` |")
    [void]$sb.AppendLine("")

    # Summary by crate/group for Modified files
    [void]$sb.AppendLine("## Карта швов по крейтам (изменённые файлы апстрима)")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("| Крейт / папка | Файлов ``M`` | Швов (hunks) | Строк ``+ / -`` |")
    [void]$sb.AppendLine("|---|---:|---:|---:|")

    $modGroups = $modifiedFiles | Group-Object { Get-GroupKey $_ } | Sort-Object Name
    foreach ($g in $modGroups) {
        $gHunks = 0
        $gAdd = 0
        $gDel = 0
        foreach ($f in $g.Group) {
            $gHunks += $fileHunks[$f].Count
            $s = $statsMap[$f]
            if ($s.Added -match '^\d+$')   { $gAdd += [int]$s.Added }
            if ($s.Removed -match '^\d+$') { $gDel += [int]$s.Removed }
        }
        [void]$sb.AppendLine("| ``$($g.Name)`` | $($g.Count) | $gHunks | ``+$gAdd / -$gDel`` |")
    }
    [void]$sb.AppendLine("")

    # Detailed file-by-file, line-by-line section for Modified files
    [void]$sb.AppendLine("## 1. Изменённые файлы апстрима — пофайлово и построчно (``M``)")
    [void]$sb.AppendLine("")
    foreach ($g in $modGroups) {
        [void]$sb.AppendLine("### ``$($g.Name)``")
        [void]$sb.AppendLine("")
        foreach ($f in $g.Group) {
            $s = $statsMap[$f]
            $hList = $fileHunks[$f]
            if ($f -eq 'Cargo.lock') {
                [void]$sb.AppendLine("- **``$f``** (``+$($s.Added) / -$($s.Removed)``) — автогенерируемый lockfile, швы не разворачиваются.")
                continue
            }
            [void]$sb.AppendLine("#### ``$f`` (``+$($s.Added) / -$($s.Removed)``, $($hList.Count) швов)")
            foreach ($h in $hList) {
                $ctxPart = if (-not [string]::IsNullOrWhiteSpace($h.Context)) {
                    $cleanCtx = $h.Context.Replace('`', "'").Replace('|', '\|')
                    if ($cleanCtx.Length -gt 80) { $cleanCtx = $cleanCtx.Substring(0, 77) + "..." }
                    " · ``$cleanCtx``"
                } else { "" }

                $prevPart = if (-not [string]::IsNullOrWhiteSpace($h.Preview)) {
                    $cleanPrev = $h.Preview.Replace('`', "'")
                    if ($cleanPrev.Length -gt 95) { $cleanPrev = $cleanPrev.Substring(0, 92) + "..." }
                    " — ``$cleanPrev``"
                } else { "" }

                [void]$sb.AppendLine("- **``$($h.RangeLabel)``** (``+$($h.NewCount)/-$($h.OldCount)``)$ctxPart$prevPart")
                if ($IncludePatch -and $h.PatchLines.Count -gt 0) {
                    [void]$sb.AppendLine('```diff')
                    foreach ($pl in $h.PatchLines) {
                        [void]$sb.AppendLine($pl)
                    }
                    [void]$sb.AppendLine('```')
                }
            }
            [void]$sb.AppendLine("")
        }
    }

    # Section 2: Added standalone files outside suitcase
    [void]$sb.AppendLine("## 2. Новые файлы вне чемодана (``A`` — без конфликтов при merge)")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("Эти файлы добавлены целиком нами (в апстриме их нет), поэтому при обновлении PhotoCraft они обычно не дают merge-конфликтов, но могут потребовать адаптации при смене внутренних API.")
    [void]$sb.AppendLine("")
    [void]$sb.AppendLine("| Файл | Строк |")
    [void]$sb.AppendLine("|---|---:|")
    foreach ($f in $addedFiles) {
        $s = $statsMap[$f]
        [void]$sb.AppendLine("| ``$f`` | ``+$($s.Added)`` |")
    }
    [void]$sb.AppendLine("")

    if ($otherFiles.Count -gt 0) {
        [void]$sb.AppendLine("## 3. Прочие изменения (``D`` / ``R``)")
        [void]$sb.AppendLine("")
        foreach ($f in $otherFiles) {
            [void]$sb.AppendLine("- ``$($statusMap[$f])`` ``$f``")
        }
        [void]$sb.AppendLine("")
    }

    $outFullPath = Join-Path $repoRoot $OutFile
    $outDir = Split-Path -Parent $outFullPath
    if (-not (Test-Path $outDir)) {
        New-Item -ItemType Directory -Path $outDir -Force | Out-Null
    }
    [System.IO.File]::WriteAllText($outFullPath, $sb.ToString(), (New-Object System.Text.UTF8Encoding($false)))

    Write-Host "Wrote $OutFile"
    Write-Host "  Modified upstream files: $($modifiedFiles.Count) ($totalHunks hunks, +$totalModAdded / -$totalModRemoved)"
    Write-Host "  Added files outside suitcase: $($addedFiles.Count)"
} finally {
    Pop-Location
}
