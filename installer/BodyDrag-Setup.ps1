# Body Drag for The Saboteur - install / uninstall.
# Usage (from the BodyDrag folder inside the game folder):  Install BodyDrag.bat  /  Uninstall BodyDrag.bat
# The animation pack is BUILT from your own DLC\01\Animations.pack (retail, hash-checked): the mod ships only its own
# animations (bodydrag_anims.subpack) and appends them as a second sub-pack; your retail file is backed up first.
param(
    [Parameter(Mandatory = $true)][ValidateSet('install', 'uninstall')][string]$Action,
    [string]$Game = ''
)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if ($Game -eq '') { $Game = Split-Path -Parent $here }

$RETAIL_PACK = 'db72130b0f53f994f93fcd58af443516deab90342077678fa77a90d963e866f5'
$MOD_PACK = '44d290cc676cf481c256e8e3a31df9d4d301a8b3aa0a3d111927a7d7afe818cd'
$SUBPACK = '9ddb1fd2d13a8b9697000798f924809476a2eda09d5bf5f65362b8e0e8361417'

function Sha($p) { if (Test-Path -LiteralPath $p) { (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLower() } else { $null } }
function Fail($msg) { Write-Host ''; Write-Host "NOT DONE: $msg" -ForegroundColor Red; exit 1 }

$exe = Join-Path $Game 'Saboteur.exe'
$pack = Join-Path $Game 'DLC\01\Animations.pack'
$asiDir = Join-Path $Game 'scripts'
$asi = Join-Path $asiDir 'sab_asi.asi'
$backupDir = Join-Path $here 'backup'
$backup = Join-Path $backupDir 'Animations.pack.retail'

if (-not (Test-Path -LiteralPath $exe)) { Fail "Saboteur.exe not found in '$Game'. Put the BodyDrag folder inside the game folder (next to Saboteur.exe), or run: BodyDrag-Setup.ps1 -Action $Action -Game '<game folder>'" }

if ($Action -eq 'install') {
    $sub = Join-Path $here 'bodydrag_anims.subpack'
    $plugin = Join-Path $here 'sab_asi.asi'
    if ((Sha $sub) -ne $SUBPACK) { Fail 'bodydrag_anims.subpack is missing or damaged - download the mod again.' }
    if (-not (Test-Path -LiteralPath $plugin)) { Fail 'sab_asi.asi is missing - download the mod again.' }

    $have = Sha $pack
    if ($have -eq $MOD_PACK) {
        Write-Host 'Animations: already installed.'
    } elseif ($have -eq $RETAIL_PACK) {
        New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
        if ((Sha $backup) -ne $RETAIL_PACK) { Copy-Item -LiteralPath $pack -Destination $backup -Force }
        if ((Sha $backup) -ne $RETAIL_PACK) { Fail 'could not back up DLC\01\Animations.pack.' }
        $retail = [IO.File]::ReadAllBytes($pack)
        $add = [IO.File]::ReadAllBytes($sub)
        if ([BitConverter]::ToUInt32($retail, 8) -ne 1 -or [BitConverter]::ToUInt32($retail, 4) -ne 256) { Fail 'unexpected DLC pack layout.' }
        $out = New-Object byte[] ($retail.Length + $add.Length)
        [Array]::Copy($retail, 0, $out, 0, $retail.Length)
        [Array]::Copy($add, 0, $out, $retail.Length, $add.Length)
        [Array]::Copy([BitConverter]::GetBytes([uint32]2), 0, $out, 8, 4)               # sub-pack count 1 -> 2
        [Array]::Copy([BitConverter]::GetBytes([uint32]$add.Length), 0, $out, 16, 4)    # size of the new sub-pack
        $tmp = "$pack.bodydrag-tmp"
        [IO.File]::WriteAllBytes($tmp, $out)
        if ((Sha $tmp) -ne $MOD_PACK) { Remove-Item -LiteralPath $tmp -Force; Fail 'the built pack did not verify; nothing changed.' }
        Move-Item -LiteralPath $tmp -Destination $pack -Force
        Write-Host 'Animations: installed (your retail DLC pack is backed up in BodyDrag\backup).'
    } elseif ($null -eq $have) {
        Fail "DLC\01\Animations.pack not found. This mod needs the retail game with its DLC folder (GOG release)."
    } else {
        Fail "DLC\01\Animations.pack is not the retail file (another mod or a different game version changed it). Restore the original file first."
    }

    New-Item -ItemType Directory -Force -Path $asiDir | Out-Null
    if ((Test-Path -LiteralPath $asi) -and ((Sha $asi) -ne (Sha $plugin))) { Copy-Item -LiteralPath $asi -Destination "$asi.previous" -Force }
    Copy-Item -LiteralPath $plugin -Destination $asi -Force
    Write-Host 'Plugin: scripts\sab_asi.asi installed.'

    $loaders = 'dinput8.dll', 'dsound.dll', 'winmm.dll', 'version.dll', 'd3d9.dll', 'xinput1_3.dll' | Where-Object { Test-Path -LiteralPath (Join-Path $Game $_) }
    if (-not $loaders) {
        Write-Host ''
        Write-Host 'WARNING: no ASI loader found in the game folder. Install "Ultimate ASI Loader" (dinput8.dll) - see the mod page requirements - or the mod will not load.' -ForegroundColor Yellow
    }
    Write-Host ''
    Write-Host 'Done. In game: stand over a body and press F7 (prompt "Pick up body [F7]"); F7 again to put it down.' -ForegroundColor Green
} else {
    $have = Sha $pack
    if ($have -eq $MOD_PACK) {
        if ((Sha $backup) -ne $RETAIL_PACK) { Fail 'the retail backup (BodyDrag\backup\Animations.pack.retail) is missing or damaged; reinstall the game file from GOG (verify files).' }
        Copy-Item -LiteralPath $backup -Destination $pack -Force
        if ((Sha $pack) -ne $RETAIL_PACK) { Fail 'restoring the DLC pack did not verify.' }
        Write-Host 'Animations: retail DLC pack restored.'
    } elseif ($have -eq $RETAIL_PACK) {
        Write-Host 'Animations: already retail.'
    } else {
        Write-Host 'Animations: DLC pack is neither retail nor this mod''s; left untouched.' -ForegroundColor Yellow
    }
    if (Test-Path -LiteralPath $asi) { Remove-Item -LiteralPath $asi -Force; Write-Host 'Plugin: scripts\sab_asi.asi removed.' }
    Write-Host ''
    Write-Host 'Uninstalled. The ASI loader and any sab_bodydrag*.ini settings files were left in place.' -ForegroundColor Green
}
