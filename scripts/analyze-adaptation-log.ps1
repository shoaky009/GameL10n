param(
    [Parameter(Mandatory)][string]$ProjectDirectory,
    [string]$RunId,
    [Nullable[datetimeoffset]]$Since,
    [datetimeoffset]$Until = [datetimeoffset]::Now,
    [ValidateRange(0, 2147483647)][Nullable[int]]$FromTurn,
    [ValidateRange(0, 2147483647)][Nullable[int]]$ToTurn,
    [switch]$Detail
)
$ErrorActionPreference = 'Stop'
if ($Since -and $Since -gt $Until) { throw 'Since must not be later than Until.' }
if ($null -ne $FromTurn -and $null -ne $ToTurn -and $FromTurn -gt $ToTurn) { throw 'FromTurn must not be later than ToTurn.' }
$logDirectory = Join-Path ([IO.Path]::GetFullPath($ProjectDirectory)) 'logs'
$invalidLines = 0
$rows = @(foreach ($file in (Get-ChildItem -LiteralPath $logDirectory -Filter 'adaptation-*.jsonl' -File | Sort-Object Name)) {
    # Bound each read to its initial length, even when another process is appending.
    $stream = [IO.File]::Open($file.FullName, 'Open', 'Read', [IO.FileShare]::ReadWrite)
    $snapshot = [IO.MemoryStream]::new()
    try {
        $remaining = $stream.Length
        $buffer = [byte[]]::new(65536)
        while ($remaining -gt 0) {
            $count = $stream.Read($buffer, 0, [int][Math]::Min($buffer.Length, $remaining))
            if ($count -eq 0) { break }
            $snapshot.Write($buffer, 0, $count)
            $remaining -= $count
        }
        $snapshot.Position = 0
        $reader = [IO.StreamReader]::new($snapshot)
        try {
            while ($null -ne ($line = $reader.ReadLine())) {
                if ([string]::IsNullOrWhiteSpace($line)) { continue }
                try { $row = $line | ConvertFrom-Json; $at = ([datetimeoffset]$row.at) }
                catch { $invalidLines++; continue }
                if ($at -le $Until) { $row }
            }
        }
        finally { $reader.Dispose() }
    }
    finally { $stream.Dispose(); $snapshot.Dispose() }
})
$rows = @($rows | Sort-Object { ([datetimeoffset]$_.at) })

function Sum-Field($items, [string]$field) {
    $values = @($items | ForEach-Object { if ($null -ne $_.$field) { $_.$field } })
    if ($values.Count -gt 0) { [double](($values | Measure-Object -Sum).Sum) }
    elseif (@($items).Count -eq 0) { 0 }
}
function Get-Usage($entry) {
    if ($null -ne $entry.usage.inputTokens -and $null -ne $entry.usage.outputTokens) { $entry.usage }
    elseif ($null -ne $entry.inputTokens -and $null -ne $entry.outputTokens) { $entry }
}
function Seconds($milliseconds) {
    if ($null -ne $milliseconds) { [Math]::Round($milliseconds / 1000, 2) }
}
function Limit-Text($value, [int]$length = 240) {
    $text = [string]$value
    $text.Substring(0, [Math]::Min($length, $text.Length))
}
function Event-Reference($row) {
    [ordered]@{ at = $row.at; runId = $row.runId; turn = $row.turn; callId = $row.entry.call.id }
}

# Host steps use the same verifier but have no model tool-call ID.
$hostStarts = @{}
foreach ($row in $rows) {
    $entry = $row.entry
    if ($entry.operation -notin @('host-step-start', 'host-step-end')) { continue }
    $key = [string]$row.runId + ':' + [string]$entry.fingerprint
    if ($entry.operation -eq 'host-step-start') {
        $hostStarts[$key] = $row
        continue
    }
    $start = $hostStarts[$key]
    $selectors = if ($null -ne $entry.selectors) { $entry.selectors } else { $start.entry.selectors }
    $entry | Add-Member -NotePropertyName tool -NotePropertyValue $entry.step -Force
    $entry | Add-Member -NotePropertyName caller -NotePropertyValue 'host' -Force
    $entry | Add-Member -NotePropertyName call -NotePropertyValue ([pscustomobject]@{ name = $entry.step; selectors = $selectors }) -Force
    if ($entry.passed -is [bool]) {
        $entry | Add-Member -NotePropertyName status -NotePropertyValue $(if ($entry.passed) { 'completed' } else { 'failed' }) -Force
    }
    if ($null -eq $entry.result) {
        $entry | Add-Member -NotePropertyName result -NotePropertyValue ([pscustomobject]@{}) -Force
        foreach ($field in @('stage', 'passed', 'issues', 'error')) {
            if ($null -ne $entry.$field) { $entry.result | Add-Member -NotePropertyName $field -NotePropertyValue $entry.$field -Force }
        }
    }
    $hostStarts.Remove($key)
}

# Rust logs tool starts and ends separately; only ends count as completed calls.
# Keep selectors from starts when the matching end does not repeat them.
$toolStarts = @{}
foreach ($row in $rows) {
    $entry = $row.entry
    if ($entry.operation -notin @('tool-start', 'tool-end')) { continue }
    $key = [string]$row.runId + ':' + [string]$entry.call.id
    if ($entry.operation -eq 'tool-start') {
        if ($entry.call.id) { $toolStarts[$key] = $row }
        continue
    }
    $start = if ($entry.call.id) { $toolStarts[$key] }
    if ($start) {
        foreach ($field in @('name', 'selectors', 'argumentFingerprint')) {
            if ($null -eq $entry.call.$field -and $null -ne $start.entry.call.$field) {
                $entry.call | Add-Member -NotePropertyName $field -NotePropertyValue $start.entry.call.$field -Force
            }
        }
        $toolStarts.Remove($key)
    }
    if (-not $entry.tool) { $entry | Add-Member -NotePropertyName tool -NotePropertyValue $entry.call.name -Force }
    if (-not $entry.status -and $entry.success -is [bool]) {
        $entry | Add-Member -NotePropertyName status -NotePropertyValue $(if ($entry.success) { 'completed' } else { 'failed' }) -Force
    }
}

# Recovery events describe the previous failed attempt, not another model call.
# Associate before applying Since so a window cannot count a retry as a new attempt.
$attempts = [Collections.Generic.List[object]]::new()
$lastAttempt = @{}
$unmatchedUsageEvents = @()
foreach ($row in $rows) {
    $key = [string]$row.runId
    if ($row.entry.operation -eq 'model') {
        $attempt = [pscustomobject]@{ row = $row; usage = $null; usageSource = $null }
        $usage = Get-Usage $row.entry
        if ($usage) { $attempt.usage = $usage; $attempt.usageSource = 'model' }
        $attempts.Add($attempt)
        $lastAttempt[$key] = $attempt
    }
    elseif ($row.entry.operation -in @('output-limit-recovery', 'model-retry') -and ($usage = Get-Usage $row.entry)) {
        $previous = $lastAttempt[$key]
        if ($previous -and $previous.row.turn -eq $row.turn -and $previous.row.entry.status -in @('failed', 'cancelled')) {
            if (-not $previous.usage) { $previous.usage = $usage; $previous.usageSource = $row.entry.operation }
        }
        else { $unmatchedUsageEvents += $row }
    }
}
function In-Window($row) {
    (-not $RunId -or $row.runId -eq $RunId) -and (-not $Since -or ([datetimeoffset]$row.at) -ge $Since) -and
        ($null -eq $FromTurn -or $row.turn -ge $FromTurn) -and ($null -eq $ToTurn -or $row.turn -le $ToTurn)
}
$rows = @($rows | Where-Object { In-Window $_ })
if ($rows.Count -eq 0) { throw 'No adaptation timing events found in the selected window.' }
$attempts = @($attempts | Where-Object { In-Window $_.row })
$modelRows = @($attempts | ForEach-Object { $_.row })
$toolRows = @($rows | Where-Object { $_.entry.operation -in @('tool', 'tool-end', 'host-step-end') })

function Usage-Summary($items) {
    $known = @($items | Where-Object { $_.usage })
    $entries = @($known | ForEach-Object { $_.usage })
    $cacheEntries = @($entries | Where-Object { $null -ne $_.cachedTokens -and $_.estimated -ne $true })
    $cacheInput = Sum-Field $cacheEntries 'inputTokens'
    $cached = if ($cacheEntries.Count -gt 0) { Sum-Field $cacheEntries 'cachedTokens' } else { $null }
    [ordered]@{
        attempts = @($items).Count
        failed = @($items | Where-Object { $_.row.entry.status -in @('failed', 'cancelled') }).Count
        inputTokens = if ($known.Count -gt 0 -or @($items).Count -eq 0) { Sum-Field $entries 'inputTokens' } else { $null }
        outputTokens = if ($known.Count -gt 0 -or @($items).Count -eq 0) { Sum-Field $entries 'outputTokens' } else { $null }
        cachedTokens = $cached
        uncachedTokens = if ($null -ne $cached) { $cacheInput - $cached } else { $null }
        cacheMeasuredInputTokens = $cacheInput
        cacheHitRate = if ($cacheInput -gt 0) { [Math]::Round($cached / $cacheInput, 4) } else { $null }
        cacheUsageRequests = $cacheEntries.Count
        missingCacheUsage = @($items).Count - $cacheEntries.Count
        recoveredFailureUsage = @($known | Where-Object { $_.usageSource -ne 'model' }).Count
        missingUsage = @($items).Count - $known.Count
        zeroUsageFields = @($known | Where-Object { $_.usage.inputTokens -eq 0 -or $_.usage.outputTokens -eq 0 }).Count
    }
}
function Tool-Reference($row) {
    $item = Event-Reference $row
    $item.tool = $row.entry.tool
    $item.caller = if ($row.entry.caller -eq 'host') { 'host' } else { 'model' }
    $item.seconds = Seconds $row.entry.elapsedMs
    $item.status = $row.entry.status
    if ($null -ne $row.entry.outputBytes) { $item.outputBytes = $row.entry.outputBytes }
    if ($row.entry.call.selectors.path) { $item.path = $row.entry.call.selectors.path }
    if ($row.entry.result.stage) { $item.stage = $row.entry.result.stage }
    if ($row.entry.error -match '\[([a-z][a-z0-9-]+)\]') { $item.validationStage = $Matches[1] }
    if ($row.entry.result.phase) { $item.phase = $row.entry.result.phase }
    if ($null -ne $row.entry.result.timedOut) { $item.timedOut = $row.entry.result.timedOut }
    if ($null -ne $row.entry.result.passed) { $item.passed = $row.entry.result.passed }
    foreach ($field in @('exitCode', 'reportError', 'scriptSha256', 'revision', 'report')) {
        if ($null -ne $row.entry.result.$field) { $item[$field] = $row.entry.result.$field }
    }
    if ($row.entry.tool -eq 'test_adapter') {
        foreach ($field in @('reused', 'compileMs', 'discoveryMs', 'verificationMs', 'fingerprintMs', 'fingerprintStats', 'verificationStagesMs')) {
            if ($null -ne $row.entry.result.$field) { $item[$field] = $row.entry.result.$field }
        }
    }
    if ($row.entry.error) { $item.error = Limit-Text $row.entry.error }
    elseif ($row.entry.result.error) { $item.error = Limit-Text $row.entry.result.error }
    $item
}
function History-Transition($row) {
    $before = $modelRows | Where-Object { $_.runId -eq $row.runId -and ([datetimeoffset]$_.at) -lt ([datetimeoffset]$row.at) } | Select-Object -Last 1
    $after = $modelRows | Where-Object { $_.runId -eq $row.runId -and ([datetimeoffset]$_.at) -gt ([datetimeoffset]$row.at) } | Select-Object -First 1
    $item = Event-Reference $row
    if ($row.entry.reason) { $item.reason = $row.entry.reason }
    $item.beforeVisibleBytes = $before.entry.visibleHistoryBytes; $item.afterVisibleBytes = $after.entry.visibleHistoryBytes
    $item.beforeRetainedBytes = $before.entry.retainedHistoryBytes; $item.afterRetainedBytes = $after.entry.retainedHistoryBytes
    $beforeUsage = if ($before) { Get-Usage $before.entry }
    $afterUsage = if ($after) { Get-Usage $after.entry }
    $item.beforeInputTokens = $beforeUsage.inputTokens; $item.afterInputTokens = $afterUsage.inputTokens
    $item.beforeCachedTokens = $beforeUsage.cachedTokens; $item.afterCachedTokens = $afterUsage.cachedTokens
    $item
}
$tools = @($toolRows | Group-Object { $_.entry.tool } | ForEach-Object {
    $entries = @($_.Group | ForEach-Object { $_.entry })
    [pscustomobject]@{ tool = $_.Name; calls = $_.Count
        seconds = Seconds (Sum-Field $entries 'elapsedMs')
        failures = @($entries | Where-Object { $_.status -eq 'failed' }).Count
        outputBytes = Sum-Field $entries 'outputBytes'
        resultCharacters = Sum-Field $entries 'resultCharacters' }
} | Sort-Object seconds -Descending)
$runs = @($rows | Where-Object { $_.runId } | Group-Object runId | ForEach-Object {
    $group = $_.Group
    $start = $group | Where-Object { $_.entry.operation -eq 'run-start' } | Select-Object -First 1
    $modelStart = $group | Where-Object { $_.entry.operation -eq 'model-start' } | Select-Object -First 1
    $end = $group | Where-Object { $_.entry.operation -eq 'run-end' } | Select-Object -Last 1
    $runAttempts = @($attempts | Where-Object { $_.row.runId -eq $group[0].runId })
    $item = [ordered]@{ runId = $_.Name; firstEvent = $group[0].at; lastEvent = $group[-1].at
        status = if ($end) { $end.entry.status } else { 'no-run-end-observed' }
        usage = Usage-Summary $runAttempts }
    if ($start) {
        $item.model = if ($modelStart) { $modelStart.entry.model } else { $start.entry.model }
        $item.maximumTokens = $start.entry.maximumTokens
        $item.maximumTurns = $start.entry.maximumTurns
        $item.fixedInstructionBytes = $start.entry.fixedInstructionBytes
        $item.registeredToolCount = $start.entry.registeredToolCount
    }
    if ($end) {
        $hostUsage = Get-Usage $end.entry
        $item.hostRunTokens = if ($null -ne $end.entry.tokens) { $end.entry.tokens }
            elseif ($hostUsage) { $hostUsage.inputTokens + $hostUsage.outputTokens } else { $null }
        if ($hostUsage) {
            $item.hostRunUsage = [ordered]@{ inputTokens = $hostUsage.inputTokens; outputTokens = $hostUsage.outputTokens
                cachedTokens = $hostUsage.cachedTokens; estimated = $hostUsage.estimated }
        }
    }
    [pscustomobject]$item
} | Sort-Object firstEvent)
$writes = @($toolRows | Where-Object { $_.entry.tool -in @('write_file', 'replace_text') -and $_.entry.status -eq 'completed' })
$sourceWrites = @($writes | Where-Object { $_.entry.call.selectors.path -match '^src[\\/]' -and $_.entry.result.changed -ne $false })
function Verification-Scope($row) {
    $selectors = $row.entry.call.selectors
    $stage = $row.entry.result.stage
    if ($selectors.discoveryOnly -eq $true -or $stage -eq 'discovery') { 'discovery' }
    elseif ($stage -eq 'trial' -or $selectors.trialFilesCount -gt 0 -or ($null -ne $selectors.trialFiles -and @($selectors.trialFiles).Count -gt 0)) { 'trial' }
    elseif ($stage -in @('full', 'verification', 'finish-ready') -or $null -ne $selectors) { 'full' }
    else { 'unknown' }
}
$verificationTests = @($toolRows | Where-Object { $_.entry.tool -eq 'test_adapter' })
$fullTests = @($verificationTests | Where-Object { (Verification-Scope $_) -eq 'full' })
$lastTest = $fullTests | Select-Object -Last 1
$prefixRows = @($modelRows | Where-Object { $null -ne $_.entry.contextComponents.requestPrefix })
$result = [ordered]@{
    project = [IO.Path]::GetFullPath($ProjectDirectory)
    window = [ordered]@{ since = $Since; until = $Until.ToString('o'); fromTurn = $FromTurn; toTurn = $ToTurn; firstEvent = $rows[0].at; lastEvent = $rows[-1].at }
    usage = Usage-Summary $attempts
    requestPrefixes = [ordered]@{
        preserved = @($prefixRows | Where-Object { $_.entry.contextComponents.requestPrefix.preserved -eq $true }).Count
        changed = @($prefixRows | Where-Object { $_.entry.contextComponents.requestPrefix.preserved -eq $false }).Count
        boundaries = @($prefixRows | Where-Object { $null -eq $_.entry.contextComponents.requestPrefix.preserved }).Count
        missing = $modelRows.Count - $prefixRows.Count
    }
    timing = [ordered]@{
        modelSeconds = Seconds (Sum-Field @($modelRows | ForEach-Object { $_.entry }) 'elapsedMs')
        toolSeconds = Seconds (Sum-Field @($toolRows | ForEach-Object { $_.entry }) 'elapsedMs')
        toolCalls = $toolRows.Count
        modelToolCalls = @($toolRows | Where-Object { $_.entry.caller -ne 'host' }).Count
        hostSteps = @($toolRows | Where-Object { $_.entry.caller -eq 'host' }).Count
        rejectedToolBatches = @($rows | Where-Object { $_.entry.operation -eq 'tool-batch-rejected' }).Count
    }
    coverage = [ordered]@{
        skippedInvalidLines = $invalidLines
        legacyRows = @($rows | Where-Object { -not $_.schemaVersion -or $_.schemaVersion -lt 2 }).Count
        unmatchedUsageEvents = @($unmatchedUsageEvents | Where-Object { In-Window $_ }).Count
        toolStartsWithoutEnd = @($toolStarts.Values | Where-Object { In-Window $_ }).Count
        hostStartsWithoutEnd = @($hostStarts.Values | Where-Object { In-Window $_ }).Count
        missingToolSelectors = @($toolRows | Where-Object { $null -eq $_.entry.call.selectors }).Count
        missingModelHistory = @($modelRows | Where-Object { $null -eq $_.entry.visibleHistoryBytes }).Count
        missingContextComponents = @($modelRows | Where-Object { $null -eq $_.entry.contextComponents }).Count
        note = 'Tokens are recorded counters, including associated failed-attempt recovery usage once; absent/zero usage is not estimated. Host run totals may be cumulative across resumed runs and include estimates. Tool starts are not counted again as calls. Missing selectors/stage leave verification scope unknown. Missing run-end does not establish activity. Invalid lines include an incomplete tail; metadata does not prove semantic progress.'
    }
    runCount = $runs.Count
    runs = @($runs | Select-Object -Last 6 | ForEach-Object {
        [ordered]@{ runId = $_.runId; status = $_.status; model = $_.model; usage = $_.usage }
    })
    milestones = [ordered]@{
        sourceEdits = $sourceWrites.Count
        writesWithoutPath = @($writes | Where-Object { -not $_.entry.call.selectors.path }).Count
        verificationCalls = $verificationTests.Count
        fullVerificationCalls = $fullTests.Count
        discoveryVerificationCalls = @($verificationTests | Where-Object { (Verification-Scope $_) -eq 'discovery' }).Count
        trialVerificationCalls = @($verificationTests | Where-Object { (Verification-Scope $_) -eq 'trial' }).Count
        unknownVerificationCalls = @($verificationTests | Where-Object { (Verification-Scope $_) -eq 'unknown' }).Count
        checkpoints = @($toolRows | Where-Object { $_.entry.tool -eq 'checkpoint' -and $_.entry.status -eq 'completed' }).Count
        checkpointRequests = @($rows | Where-Object { $_.entry.operation -eq 'checkpoint-request' }).Count
        automaticCompactions = @($rows | Where-Object { $_.entry.operation -eq 'history-compacted' }).Count
        implementationReviews = @($rows | Where-Object { $_.entry.operation -eq 'implementation-review' }).Count
        lastSourceEdit = if ($sourceWrites.Count) { Tool-Reference $sourceWrites[-1] } else { $null }
        lastFullVerification = if ($lastTest) { Tool-Reference $lastTest } else { $null }
        modelCallsSinceLastVerification = if ($lastTest) { @($modelRows | Where-Object { ([datetimeoffset]$_.at) -gt ([datetimeoffset]$lastTest.at) }).Count } else { $null }
    }
    topToolsByTime = @($tools | Select-Object -First 5)
    slowestToolCalls = @($toolRows | Sort-Object { $_.entry.elapsedMs } -Descending | Select-Object -First 3 | ForEach-Object { Tool-Reference $_ })
    largestToolOutputs = @($toolRows | Where-Object { $_.entry.outputBytes -gt 0 } | Sort-Object { $_.entry.outputBytes } -Descending | Select-Object -First 3 | ForEach-Object { Tool-Reference $_ })
    recentFailures = @($rows | Where-Object { $_.entry.status -in @('failed', 'cancelled') -and $_.entry.operation -in @('model', 'tool', 'tool-end', 'host-step-end') } | Select-Object -Last 3 | ForEach-Object {
        if ($_.entry.operation -in @('tool', 'tool-end', 'host-step-end')) { Tool-Reference $_ }
        else {
            $item = Event-Reference $_
            $item.operation = 'model'; $item.status = $_.entry.status; $item.failureCode = $_.entry.failureCode
            $item.seconds = Seconds $_.entry.elapsedMs
            $item
        }
    })
}
if ($Detail) {
    $result.runs = $runs
    $result.tools = $tools
    $result.repeatedArguments = @($toolRows | Where-Object { $_.entry.call.argumentFingerprint } |
        Group-Object { $_.entry.tool + ':' + $_.entry.call.argumentFingerprint } | Where-Object Count -gt 1 |
        Sort-Object Count -Descending | Select-Object -First 20 | ForEach-Object {
            [pscustomobject]@{ tool = $_.Group[0].entry.tool; calls = $_.Count
                selectors = $_.Group[0].entry.call.selectors
                callIds = @($_.Group | Select-Object -First 4 | ForEach-Object { $_.entry.call.id }) }
        })
    $result.identicalResults = @($toolRows | Where-Object { $_.entry.resultFingerprint } |
        Group-Object { $_.entry.tool + ':' + $_.entry.resultFingerprint } | Where-Object Count -gt 1 |
        Sort-Object Count -Descending | Select-Object -First 12 | ForEach-Object {
            [pscustomobject]@{ tool = $_.Group[0].entry.tool; calls = $_.Count
                resultFingerprint = $_.Group[0].entry.resultFingerprint
                callIds = @($_.Group | Select-Object -First 4 | ForEach-Object { $_.entry.call.id }) }
        })
    $result.rejectedBatches = @($rows | Where-Object { $_.entry.operation -eq 'tool-batch-rejected' } | ForEach-Object {
        $item = Event-Reference $_
        $item.tools = $_.entry.tools; $item.error = $_.entry.error
        $item
    })
    $result.checkpointHistory = @($toolRows | Where-Object { $_.entry.tool -eq 'checkpoint' -and $_.entry.status -eq 'completed' } |
        Select-Object -Last 3 | ForEach-Object { History-Transition $_ })
    $result.historyCompactions = @($rows | Where-Object { $_.entry.operation -eq 'history-compacted' } |
        Select-Object -Last 6 | ForEach-Object { History-Transition $_ })
    $result.modelTurns = @($attempts | ForEach-Object {
        $item = Event-Reference $_.row
        $item.status = $_.row.entry.status; $item.usageSource = $_.usageSource
        $item.inputTokens = $_.usage.inputTokens; $item.outputTokens = $_.usage.outputTokens
        $item.cachedTokens = $_.usage.cachedTokens
        $item.uncachedTokens = if ($null -ne $_.usage.cachedTokens -and $_.usage.estimated -ne $true) { $_.usage.inputTokens - $_.usage.cachedTokens } else { $null }
        $item.seconds = Seconds $_.row.entry.elapsedMs
        $item.visibleHistoryBytes = $_.row.entry.visibleHistoryBytes
        $item.instructionBytes = $_.row.entry.instructionBytes; $item.toolDefinitionBytes = $_.row.entry.toolDefinitionBytes
        $item.instructionFingerprint = $_.row.entry.instructionFingerprint
        $item.toolDefinitionFingerprint = $_.row.entry.toolDefinitionFingerprint
        $item.contextComponents = $_.row.entry.contextComponents
        $item.verificationConfigured = $_.row.entry.verificationConfigured
        $item
    })
}
$result | ConvertTo-Json -Depth 12 -Compress:(-not $Detail)
