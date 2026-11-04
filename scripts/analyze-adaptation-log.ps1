param(
    [Parameter(Mandatory)][string]$ProjectDirectory,
    [string]$RunId,
    [Nullable[datetimeoffset]]$Since,
    [datetimeoffset]$Until = [datetimeoffset]::Now,
    [switch]$Detail
)
$ErrorActionPreference = 'Stop'
if ($Since -and $Since -gt $Until) { throw 'Since must not be later than Until.' }
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
    [double](($items | ForEach-Object { $_.$field } | Measure-Object -Sum).Sum)
}
function Has-Usage($entry) { $null -ne $entry.inputTokens -and $null -ne $entry.outputTokens }
function Limit-Text($value, [int]$length = 240) {
    $text = [string]$value
    $text.Substring(0, [Math]::Min($length, $text.Length))
}
function Event-Reference($row) {
    [ordered]@{ at = $row.at; runId = $row.runId; turn = $row.turn; callId = $row.entry.call.id }
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
        if (Has-Usage $row.entry) { $attempt.usage = $row.entry; $attempt.usageSource = 'model' }
        $attempts.Add($attempt)
        $lastAttempt[$key] = $attempt
    }
    elseif ($row.entry.operation -in @('output-limit-recovery', 'model-retry') -and (Has-Usage $row.entry)) {
        $previous = $lastAttempt[$key]
        if ($previous -and $previous.row.turn -eq $row.turn -and $previous.row.entry.status -in @('failed', 'cancelled')) {
            if (-not $previous.usage) { $previous.usage = $row.entry; $previous.usageSource = $row.entry.operation }
        }
        else { $unmatchedUsageEvents += $row }
    }
}
function In-Window($row) {
    (-not $RunId -or $row.runId -eq $RunId) -and (-not $Since -or ([datetimeoffset]$row.at) -ge $Since)
}
$rows = @($rows | Where-Object { In-Window $_ })
if ($rows.Count -eq 0) { throw 'No adaptation timing events found in the selected window.' }
$attempts = @($attempts | Where-Object { In-Window $_.row })
$modelRows = @($attempts | ForEach-Object { $_.row })
$toolRows = @($rows | Where-Object { $_.entry.operation -eq 'tool' })

function Usage-Summary($items) {
    $known = @($items | Where-Object { $_.usage })
    $entries = @($known | ForEach-Object { $_.usage })
    [ordered]@{
        attempts = @($items).Count
        failed = @($items | Where-Object { $_.row.entry.status -in @('failed', 'cancelled') }).Count
        inputTokens = Sum-Field $entries 'inputTokens'
        outputTokens = Sum-Field $entries 'outputTokens'
        recoveredFailureUsage = @($known | Where-Object { $_.usageSource -ne 'model' }).Count
        missingUsage = @($items).Count - $known.Count
        zeroUsageFields = @($known | Where-Object { $_.usage.inputTokens -eq 0 -or $_.usage.outputTokens -eq 0 }).Count
    }
}
function Tool-Reference($row) {
    $item = Event-Reference $row
    $item.tool = $row.entry.tool
    $item.seconds = [Math]::Round($row.entry.elapsedMs / 1000, 2)
    $item.status = $row.entry.status
    if ($row.entry.call.selectors.path) { $item.path = $row.entry.call.selectors.path }
    if ($row.entry.result.stage) { $item.stage = $row.entry.result.stage }
    if ($row.entry.error -match '\[([a-z][a-z0-9-]+)\]') { $item.validationStage = $Matches[1] }
    if ($row.entry.result.phase) { $item.phase = $row.entry.result.phase }
    if ($null -ne $row.entry.result.timedOut) { $item.timedOut = $row.entry.result.timedOut }
    if ($null -ne $row.entry.result.passed) { $item.passed = $row.entry.result.passed }
    if ($row.entry.error) { $item.error = Limit-Text $row.entry.error }
    $item
}
$tools = @($toolRows | Group-Object { $_.entry.tool } | ForEach-Object {
    $entries = @($_.Group | ForEach-Object { $_.entry })
    [pscustomobject]@{ tool = $_.Name; calls = $_.Count
        seconds = [Math]::Round((Sum-Field $entries 'elapsedMs') / 1000, 2)
        failures = @($entries | Where-Object { $_.status -eq 'failed' }).Count
        outputBytes = Sum-Field $entries 'outputBytes' }
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
    if ($end) { $item.hostRunTokens = $end.entry.tokens }
    [pscustomobject]$item
} | Sort-Object firstEvent)
$writes = @($toolRows | Where-Object { $_.entry.tool -in @('write_file', 'replace_text') -and $_.entry.status -eq 'completed' })
$sourceWrites = @($writes | Where-Object { $_.entry.call.selectors.path -match '^src[\\/]' -and $_.entry.result.changed -ne $false })
$fullTests = @($toolRows | Where-Object { $_.entry.tool -eq 'test_adapter' -and $_.entry.call.selectors.discoveryOnly -ne $true -and $_.entry.result.stage -ne 'discovery' })
$lastTest = $fullTests | Select-Object -Last 1
$result = [ordered]@{
    project = [IO.Path]::GetFullPath($ProjectDirectory)
    window = [ordered]@{ since = $Since; until = $Until.ToString('o'); firstEvent = $rows[0].at; lastEvent = $rows[-1].at }
    usage = Usage-Summary $attempts
    timing = [ordered]@{
        modelSeconds = [Math]::Round((Sum-Field @($modelRows | ForEach-Object { $_.entry }) 'elapsedMs') / 1000, 2)
        toolSeconds = [Math]::Round((Sum-Field @($toolRows | ForEach-Object { $_.entry }) 'elapsedMs') / 1000, 2)
        toolCalls = $toolRows.Count
        rejectedToolBatches = @($rows | Where-Object { $_.entry.operation -eq 'tool-batch-rejected' }).Count
    }
    coverage = [ordered]@{
        skippedInvalidLines = $invalidLines
        legacyRows = @($rows | Where-Object { -not $_.schemaVersion -or $_.schemaVersion -lt 2 }).Count
        unmatchedUsageEvents = @($unmatchedUsageEvents | Where-Object { In-Window $_ }).Count
        note = 'Tokens are recorded counters, including associated failed-attempt recovery usage once; absent/zero usage is not estimated. Host run totals may include estimates. Missing run-end does not establish activity. Invalid lines include an incomplete tail; metadata does not prove semantic progress.'
    }
    runCount = $runs.Count
    runs = @($runs | Select-Object -Last 6 | ForEach-Object {
        [ordered]@{ runId = $_.runId; status = $_.status; model = $_.model; usage = $_.usage }
    })
    milestones = [ordered]@{
        sourceEdits = $sourceWrites.Count
        fullVerificationCalls = $fullTests.Count
        checkpoints = @($toolRows | Where-Object { $_.entry.tool -eq 'checkpoint' -and $_.entry.status -eq 'completed' }).Count
        checkpointRequests = @($rows | Where-Object { $_.entry.operation -eq 'checkpoint-request' }).Count
        implementationReviews = @($rows | Where-Object { $_.entry.operation -eq 'implementation-review' }).Count
        lastSourceEdit = if ($sourceWrites.Count) { Tool-Reference $sourceWrites[-1] } else { $null }
        lastFullVerification = if ($lastTest) { Tool-Reference $lastTest } else { $null }
        modelCallsSinceLastVerification = if ($lastTest) { @($modelRows | Where-Object { ([datetimeoffset]$_.at) -gt ([datetimeoffset]$lastTest.at) }).Count } else { $null }
    }
    topToolsByTime = @($tools | Select-Object -First 5)
    slowestToolCalls = @($toolRows | Sort-Object { $_.entry.elapsedMs } -Descending | Select-Object -First 3 | ForEach-Object { Tool-Reference $_ })
    recentFailures = @($rows | Where-Object { $_.entry.status -in @('failed', 'cancelled') -and $_.entry.operation -in @('model', 'tool') } | Select-Object -Last 3 | ForEach-Object {
        if ($_.entry.operation -eq 'tool') { Tool-Reference $_ }
        else {
            $item = Event-Reference $_
            $item.operation = 'model'; $item.status = $_.entry.status; $item.failureCode = $_.entry.failureCode
            $item.seconds = [Math]::Round($_.entry.elapsedMs / 1000, 2)
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
    $result.checkpointHistory = @($toolRows | Where-Object { $_.entry.tool -eq 'checkpoint' -and $_.entry.status -eq 'completed' } |
        Select-Object -Last 3 | ForEach-Object {
            $checkpoint = $_
            $before = $modelRows | Where-Object { $_.runId -eq $checkpoint.runId -and ([datetimeoffset]$_.at) -lt ([datetimeoffset]$checkpoint.at) } | Select-Object -Last 1
            $after = $modelRows | Where-Object { $_.runId -eq $checkpoint.runId -and ([datetimeoffset]$_.at) -gt ([datetimeoffset]$checkpoint.at) } | Select-Object -First 1
            $item = Event-Reference $checkpoint
            $item.beforeVisibleBytes = $before.entry.visibleHistoryBytes; $item.afterVisibleBytes = $after.entry.visibleHistoryBytes
            $item.beforeRetainedBytes = $before.entry.retainedHistoryBytes; $item.afterRetainedBytes = $after.entry.retainedHistoryBytes
            $item
        })
    $result.modelTurns = @($attempts | ForEach-Object {
        $item = Event-Reference $_.row
        $item.status = $_.row.entry.status; $item.usageSource = $_.usageSource
        $item.inputTokens = $_.usage.inputTokens; $item.outputTokens = $_.usage.outputTokens
        $item.visibleHistoryBytes = $_.row.entry.visibleHistoryBytes
        $item.instructionBytes = $_.row.entry.instructionBytes; $item.toolDefinitionBytes = $_.row.entry.toolDefinitionBytes
        $item
    })
}
$result | ConvertTo-Json -Depth 12 -Compress:(-not $Detail)
