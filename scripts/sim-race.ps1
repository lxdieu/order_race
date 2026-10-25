# Chay thu racer voi hnxsim: sim phat K04 mo phien sau $Lead giay, racer ban lenh khi nhan K04.
# Lenh toi som bi sim tu choi 0101 (venue bat), nen tong ket cho biet racer ban dung luc hay khong.
#
#   .\scripts\sim-race.ps1                      # 2000 lenh, mo phien sau 45s
#   .\scripts\sim-race.ps1 -Count 10000 -Rate 3000 -Lead 30
#   .\scripts\sim-race.ps1 -ClockEarlyMs 300    # doi chung: bo K04, ban theo dong ho som 300ms
param(
    [int]$Count = 2000,
    [int]$Lead = 45,
    [double]$Rate = 2000,
    [int]$Burst = 20,
    [int]$ClockEarlyMs = 0,
    # cong cua sim = cong trong acceptor.cfg + PortOffset, de khong dung voi hnxsim dang chay cho viec khac
    [int]$PortOffset = 1000,
    [string]$SimDir = "D:\project\exchange_sim"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$run = Join-Path $root "sim\run"
$utf8 = New-Object System.Text.UTF8Encoding $false
function Write-NoBom($path, $text) { [IO.File]::WriteAllText($path, $text, $utf8) }

$simExe = Join-Path $SimDir "bin\hnxsim.exe"
if (-not (Test-Path $simExe)) { throw "khong thay $simExe (chay $SimDir\scripts\build.ps1 truoc)" }
function Port($p) { $p + $PortOffset }
$admin = "127.0.0.1:$(Port 18080)"
foreach ($port in 30119, 30120, 30112, 30122, 18080) {
    if (Get-NetTCPConnection -LocalPort (Port $port) -State Listen -ErrorAction SilentlyContinue) {
        throw "cong $(Port $port) dang bi chiem (doi -PortOffset)"
    }
}
New-Item -ItemType Directory -Force $run | Out-Null
# ban sao acceptor.cfg voi cong da dich; file goc cua exchange_sim giu nguyen
$acceptor = [regex]::Replace((Get-Content (Join-Path $SimDir "config\acceptor.cfg") -Raw), '(?m)^SocketAcceptPort=(\d+)',
    { param($m) "SocketAcceptPort=$([int]$m.Groups[1].Value + $PortOffset)" })
$acceptorPath = Join-Path $root "sim\acceptor.generated.cfg"
Write-NoBom $acceptorPath $acceptor
Remove-Item -Recurse -Force (Join-Path $run "journal") -ErrorAction SilentlyContinue

# --- gio mo phien (gio VN, lam tron len giay) ---
$vnNow = [DateTime]::UtcNow.AddHours(7)
$open = $vnNow.AddSeconds($Lead + 1).ToString("HH:mm:ss")
Write-Host "gio mo phien gia lap: $open (gio VN)"

# --- config hnxsim ---
function Ev($sess, $mkt, $evt) {
    @{ at = $open; session = $sess; market_id = $mkt; board_id = "G1"; product_grp_id = $mkt; board_evt_id = $evt }
}
$simCfg = [ordered]@{
    fix_config     = $acceptorPath
    fix_log        = $false
    admin_addr     = $admin
    dispatch       = "async"
    queue_size     = 65536
    seed           = 42
    scenario       = @{
        default      = @(@{ weight = 100; action = "ack" })
        cancel       = @{ action = "accept"; delay = "0s" }
        replace      = @{ action = "accept"; delay = "0s" }
        market_price = 10000
    }
    venue          = @{
        enabled = $true
        prices  = @(
            @{ symbol = "VN000000HCM0"; market_id = "STO"; ref = 24200 },
            @{ symbol = "VN000000SHS6"; market_id = "STX"; ref = 10000 },
            @{ symbol = "VN000000ACV1"; market_id = "UPX"; ref = 80000 }
        )
    }
    schedule       = @{
        enabled = $true; speed = 1; start_at = ""; day_start = "00:00:01"; send_prices = $false
        events  = @((Ev "VOMGW" "STO" "AA1"), (Ev "VXMGW" "STX" "BB1"), (Ev "LTHNX" "UPX" "AB1"))
    }
    session_events = @{ on_logon = $false; events = @() }
}
$simCfgPath = Join-Path $root "sim\hnxsim.race.generated.json"
Write-NoBom $simCfgPath ($simCfg | ConvertTo-Json -Depth 6)

# --- file lenh: HOSE (LO + mot it ATO), HNX, UPCoM; gia trong bien do, dung buoc gia ---
$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine("received_at,account,exchange,symbol,side,ord_type,qty,price")
$t0 = $vnNow.Date.AddHours(-4)  # 20:00 hom truoc
$rnd = New-Object System.Random 7
for ($i = 0; $i -lt $Count; $i++) {
    $ts = $t0.AddMilliseconds($i * 37).ToString("yyyy-MM-ddTHH:mm:ss.fff") + "+07:00"
    $acc = "011C{0:D6}" -f ($i % 500)
    $side = if ($rnd.Next(2) -eq 0) { "B" } else { "S" }
    $qty = 100 * (1 + $rnd.Next(20))
    switch ($i % 10) {
        { $_ -lt 5 } { $line = "$ts,$acc,HOSE,VN000000HCM0,$side,LO,$qty,$(23000 + 50 * $rnd.Next(50))" }
        5 { $line = "$ts,$acc,HOSE,VN000000HCM0,$side,ATO,$qty," }
        { $_ -in 6, 7, 8 } { $line = "$ts,$acc,HNX,VN000000SHS6,$side,LO,$qty,$(9500 + 100 * $rnd.Next(10))" }
        default { $line = "$ts,$acc,UPCOM,VN000000ACV1,$side,LO,$qty,$(75000 + 100 * $rnd.Next(100))" }
    }
    [void]$sb.AppendLine($line)
}
$ordersPath = Join-Path $root "sim\orders.generated.csv"
Write-NoBom $ordersPath $sb.ToString()

# --- env cho racer: 2 session lenh + 2 session tin (UPCoM nhan K04 tren chinh session lenh LTHNX) ---
$envText = @"
RACE_OPEN_TIME=$open
RACE_CONNECT_LEAD=20s
RACE_SPIN_LEAD=3s
RACE_GIVE_UP_AFTER=30s
RACE_ACK_TIMEOUT=10s
RACE_ORDERS_FILE=$($ordersPath -replace '\\','/')
RACE_JOURNAL_DIR=$((Join-Path $run 'journal') -replace '\\','/')
RACE_SESSIONS=hose,hnx,hose_md,hnx_md
RACE_HOSE_ADDR=127.0.0.1:$(Port 30120)
RACE_HOSE_SENDER=HOSE01
RACE_HOSE_TARGET=LTHOSE
RACE_HOSE_MARKETS=STO
RACE_HOSE_RATE=$Rate
RACE_HOSE_BURST=$Burst
RACE_HOSE_RESET_SEQ=true
RACE_HOSE_BUSY_POLL=false
RACE_HNX_ADDR=127.0.0.1:$(Port 30122)
RACE_HNX_SENDER=HNX01
RACE_HNX_TARGET=LTHNX
RACE_HNX_MARKETS=STX,UPX
RACE_HNX_RATE=$Rate
RACE_HNX_BURST=$Burst
RACE_HNX_RESET_SEQ=true
RACE_HOSE_MD_ADDR=127.0.0.1:$(Port 30119)
RACE_HOSE_MD_SENDER=HOSE02
RACE_HOSE_MD_TARGET=VOMGW
RACE_HOSE_MD_RESET_SEQ=true
RACE_HNX_MD_ADDR=127.0.0.1:$(Port 30112)
RACE_HNX_MD_SENDER=HNX02
RACE_HNX_MD_TARGET=VXMGW
RACE_HNX_MD_RESET_SEQ=true
"@
if ($ClockEarlyMs -gt 0) {
    # khong co K04 nao khop trigger; du phong 0ms sau "gio mo" da lui som $ClockEarlyMs ms
    $early = $vnNow.Date.Add([TimeSpan]::Parse($open)).AddMilliseconds(-$ClockEarlyMs).ToString("HH:mm:ss.fff")
    $envText = ($envText -replace "RACE_OPEN_TIME=.*", "RACE_OPEN_TIME=$early") + @"

RACE_TRIGGER_STO=NONE
RACE_TRIGGER_STX=NONE
RACE_TRIGGER_UPX=NONE
RACE_FALLBACK_DELAY=0
"@
    Write-Host "DOI CHUNG: ban theo dong ho luc $early, som ${ClockEarlyMs}ms so voi K04"
}
$envPath = Join-Path $root "sim\.env.generated"
Write-NoBom $envPath $envText

# --- build racer, chay sim, chay racer ---
Push-Location $root
try {
    cargo build --release --quiet
    if ($LASTEXITCODE -ne 0) { throw "cargo build that bai" }
} finally { Pop-Location }

$sim = Start-Process -FilePath $simExe -ArgumentList "-config", "`"$simCfgPath`"", "-stats-interval", "0" `
    -WorkingDirectory $run -RedirectStandardOutput (Join-Path $run "hnxsim.out.log") `
    -RedirectStandardError (Join-Path $run "hnxsim.err.log") -PassThru -WindowStyle Hidden
try {
    $up = $false
    for ($i = 0; $i -lt 50 -and -not $up; $i++) {
        Start-Sleep -Milliseconds 200
        try { Invoke-RestMethod http://$admin/venue | Out-Null; $up = $true } catch {}
        if ($sim.HasExited) { break }
    }
    if (-not $up) { Get-Content (Join-Path $run "hnxsim.err.log") -Tail 20; throw "hnxsim khong khoi dong duoc" }
    Write-Host "hnxsim da chay (pid $($sim.Id)), phien truoc gio mo:"
    (Invoke-RestMethod http://$admin/venue).markets | Format-Table -AutoSize | Out-String | Write-Host

    & (Join-Path $root "target\release\racer.exe") --env $envPath
    $racerExit = $LASTEXITCODE

    Write-Host "`n===== PHIA SIM ====="
    $stats = Invoke-RestMethod http://$admin/stats
    $stats.sessions | Format-Table -AutoSize | Out-String | Write-Host
    (Invoke-RestMethod http://$admin/venue).markets | Format-Table -AutoSize | Out-String | Write-Host
    Write-Host "racer exit code: $racerExit"
} finally {
    Stop-Process -Id $sim.Id -Force -ErrorAction SilentlyContinue
}
