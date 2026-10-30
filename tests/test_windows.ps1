# DOMO Windows smoke test.
#
# Covers:
#   1. the server builds and starts on Windows with a temporary data root;
#   2. the built-in mock planner creates a two-task DAG;
#   3. tasks run in Git worktrees and merge into the project branch;
#   4. the report is produced and can be accepted.
#
# The test uses the built-in mock agent, never calls a real model, and keeps all
# runtime data under the system temp directory. Exit code 0 means every check
# passed. Run with: powershell -ExecutionPolicy Bypass -File tests/test_windows.ps1

# Native tools write progress to stderr; keep it visible instead of turning it
# into a terminating error. Every check below asserts explicitly.
$ErrorActionPreference = "Continue"

$Root = Split-Path -Parent $PSScriptRoot
$TmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("domo-windows-" + [guid]::NewGuid().ToString("N"))
$Repo = Join-Path $TmpRoot "repo"
$OutLog = Join-Path $TmpRoot "server.out.log"
$ErrLog = Join-Path $TmpRoot "server.err.log"
$Server = $null
$EnvNames = @(
    "DOMO_DATA_ROOT",
    "DOMO_WORKSPACE_ROOT",
    "DOMO_LOG_ROOT",
    "DOMO_SERVER_PORT",
    "DOMO_MOCK_DELAY_MS"
)
$PreviousEnv = @{}

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw "Assertion failed: $Message"
    }
}

function Invoke-Api {
    param([string]$Method, [string]$Path, $Body)
    $params = @{
        Uri = "$Base$Path"
        Method = $Method
        TimeoutSec = 15
        ErrorAction = "Stop"
    }
    if ($null -ne $Body) {
        $params.ContentType = "application/json"
        $params.Body = ($Body | ConvertTo-Json -Depth 10 -Compress)
    }
    Invoke-RestMethod @params
}

function Wait-Until {
    param(
        [scriptblock]$Condition,
        [string]$Label,
        [int]$TimeoutSec = 90
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = $null
    while ((Get-Date) -lt $deadline) {
        $last = Invoke-Api -Method "GET" -Path "/api/projects/$ProjectId"
        if (& $Condition $last) {
            return $last
        }
        Start-Sleep -Milliseconds 200
    }
    throw "Timed out waiting for $Label. Last state: $($last | ConvertTo-Json -Depth 6 -Compress)"
}

try {
    New-Item -ItemType Directory -Path $Repo -Force | Out-Null

    Write-Host "==> init test repository"
    $output = & git -C $Repo init -q -b main 2>&1
    $output | ForEach-Object { Write-Host $_ }
    Assert-True ($LASTEXITCODE -eq 0) "git init failed"
    $output = & git -C $Repo -c user.name=test -c user.email=test@example.com commit --allow-empty -q -m init 2>&1
    $output | ForEach-Object { Write-Host $_ }
    Assert-True ($LASTEXITCODE -eq 0) "git commit failed"

    Write-Host "==> build server"
    $output = & cargo build --manifest-path (Join-Path $Root "server\Cargo.toml") 2>&1
    $output | ForEach-Object { Write-Host $_ }
    Assert-True ($LASTEXITCODE -eq 0) "cargo build failed"

    $listener = New-Object System.Net.Sockets.TcpListener([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $Port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    $listener.Stop()
    $Base = "http://127.0.0.1:$Port"

    $values = @{
        DOMO_DATA_ROOT = Join-Path $TmpRoot "data"
        DOMO_WORKSPACE_ROOT = Join-Path $TmpRoot "ws"
        DOMO_LOG_ROOT = Join-Path $TmpRoot "log"
        DOMO_SERVER_PORT = "$Port"
        DOMO_MOCK_DELAY_MS = "40"
    }
    foreach ($name in $EnvNames) {
        $PreviousEnv[$name] = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, $values[$name])
    }

    Write-Host "==> start server on port $Port"
    $exe = Join-Path $Root "server\target\debug\domo-server.exe"
    $Server = Start-Process -FilePath $exe -PassThru -NoNewWindow `
        -RedirectStandardOutput $OutLog -RedirectStandardError $ErrLog

    $healthy = $false
    for ($i = 0; $i -lt 150; $i++) {
        try {
            $health = Invoke-RestMethod -Uri "$Base/api/health" -TimeoutSec 1 -ErrorAction Stop
            $healthy = $true
            break
        } catch {
            if ($Server.HasExited) {
                break
            }
            Start-Sleep -Milliseconds 100
        }
    }
    if (-not $healthy) {
        $log = ""
        if (Test-Path $OutLog) { $log = Get-Content $OutLog -Raw }
        if (Test-Path $ErrLog) { $log += "`n" + (Get-Content $ErrLog -Raw) }
        throw "server did not become healthy. Log: $log"
    }
    Assert-True ($health.ok -eq $true) "health check failed"
    if (Test-Path (Join-Path $Root "web\dist\index.html")) {
        $index = Invoke-WebRequest -Uri "$Base/" -UseBasicParsing -ErrorAction Stop
        Assert-True ($index.StatusCode -eq 200) "web UI is not served"
    }

    Write-Host "==> register mock agent"
    $agents = Invoke-Api -Method "GET" -Path "/api/agents"
    foreach ($agent in $agents) {
        Invoke-Api -Method "PATCH" -Path "/api/agents/$($agent.id)" -Body @{ enabled = $false } | Out-Null
    }
    $mock = Invoke-Api -Method "POST" -Path "/api/agents" -Body @{
        name = "Windows Mock"
        kind = "mock"
        roles = @("planner", "executor", "verifier")
        max_concurrency = 2
    }

    Write-Host "==> create project and start analysis"
    $created = Invoke-Api -Method "POST" -Path "/api/projects" -Body @{
        name = "Windows Smoke"
        repo_path = $Repo
        base_branch = "main"
    }
    $ProjectId = $created.project.id
    Invoke-Api -Method "POST" -Path "/api/projects/$ProjectId/analysis/messages" -Body @{
        message = "generate two dependent mock files"
        confirm_mode = "auto"
    } | Out-Null

    $detail = Wait-Until -Label "project report" -Condition {
        param($d)
        $d.project.stage -eq "summarizing" -and -not [string]::IsNullOrWhiteSpace($d.project.report)
    }

    Assert-True ($detail.tasks.Count -eq 2) "expected two tasks, got $($detail.tasks.Count)"
    foreach ($task in $detail.tasks) {
        Assert-True ($task.stage -eq "completed") "task $($task.key) is $($task.stage)"
    }
    Assert-True ($detail.jobs.Count -gt 0) "no jobs were recorded"
    Assert-True ($detail.events.Count -gt 0) "no events were recorded"

    $branch = $detail.project.branch
    $t1 = (& git -C $Repo show "$($branch):domo_mock/T1.txt" 2>&1) -join "`n"
    Assert-True ($LASTEXITCODE -eq 0) "T1 artifact missing from $branch"
    Assert-True ($t1 -match "T1") "T1 artifact has unexpected content"
    $t2 = (& git -C $Repo show "$($branch):domo_mock/T2.txt" 2>&1) -join "`n"
    Assert-True ($LASTEXITCODE -eq 0) "T2 artifact missing from $branch"
    Assert-True ($t2 -match "dep T1 was merged") "T2 artifact has unexpected content"

    Write-Host "==> accept report"
    Invoke-Api -Method "POST" -Path "/api/projects/$ProjectId/report" -Body @{ action = "accept" } | Out-Null
    $completed = Invoke-Api -Method "GET" -Path "/api/projects/$ProjectId"
    Assert-True ($completed.project.stage -eq "completed") "project stage is $($completed.project.stage)"

    Write-Host "Windows smoke test passed"
    Write-Host "  project branch: $branch"
    Write-Host "  jobs: $($detail.jobs.Count)"
    Write-Host "  events: $($detail.events.Count)"
} finally {
    if ($Server -and -not $Server.HasExited) {
        $null = & taskkill /PID $Server.Id /T /F 2>&1
        $null = $Server.WaitForExit(5000)
    }
    foreach ($name in $EnvNames) {
        [Environment]::SetEnvironmentVariable($name, $PreviousEnv[$name])
    }
    # Windows may hold the redirected log files for a moment after exit.
    for ($attempt = 0; $attempt -lt 10 -and (Test-Path $TmpRoot); $attempt++) {
        Remove-Item -Recurse -Force $TmpRoot -ErrorAction SilentlyContinue
        if (Test-Path $TmpRoot) {
            Start-Sleep -Milliseconds 200
        }
    }
}
