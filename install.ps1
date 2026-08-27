# Desjanjador - instalador portable para Windows (sem assinatura).
#
# Uso:
#   .\install.ps1               instala em %LOCALAPPDATA%, liga o autostart e abre
#   .\install.ps1 -NoAutostart  instala sem iniciar com o Windows
#   .\install.ps1 -Uninstall    remove tudo
#
# Rodar como Administrador (opcional) adiciona uma exclusao no Windows Defender.

param(
    [switch]$Uninstall,
    [switch]$NoAutostart
)

$ErrorActionPreference = "Stop"
$AppName      = "Desjanjador"
$ExeName      = "desjanjador.exe"
$InstallDir   = Join-Path $env:LOCALAPPDATA $AppName
$InstalledExe = Join-Path $InstallDir $ExeName
$RunKey       = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$InetKey      = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
$IsAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
           ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

function Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }

if ($Uninstall) {
    Step "Removendo o $AppName..."
    Get-Process desjanjador -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-ItemProperty -Path $RunKey -Name $AppName -ErrorAction SilentlyContinue
    if ($IsAdmin) { try { Remove-MpPreference -ExclusionPath $InstallDir -ErrorAction SilentlyContinue } catch {} }
    $cur = (Get-ItemProperty $InetKey -Name AutoConfigURL -ErrorAction SilentlyContinue).AutoConfigURL
    if ($cur -like "*127.0.0.1:43110*") { Remove-ItemProperty $InetKey -Name AutoConfigURL -ErrorAction SilentlyContinue }
    Remove-Item $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Removido." -ForegroundColor Green
    return
}

$src = Join-Path $PSScriptRoot $ExeName
if (-not (Test-Path $src)) { throw "Nao encontrei '$ExeName' ao lado do instalador." }

Step "Instalando em $InstallDir"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Get-Process desjanjador -ErrorAction SilentlyContinue | Stop-Process -Force
Copy-Item $src $InstalledExe -Force
try { Unblock-File $InstalledExe } catch {}   # tira a marca da web (reduz aviso do SmartScreen)

if ($IsAdmin) {
    Step "Admin: adicionando exclusao no Windows Defender (opcional)"
    try { Add-MpPreference -ExclusionPath $InstallDir -ErrorAction Stop; Write-Host "  exclusao adicionada" -ForegroundColor Green }
    catch { Write-Host "  nao deu para adicionar exclusao (segue sem ela)" -ForegroundColor Yellow }
} else {
    Write-Host "Dica: rode como Administrador uma vez para excluir do Defender (opcional)." -ForegroundColor DarkGray
}

if (-not $NoAutostart) {
    Step "Ligando a inicializacao com o Windows"
    Set-ItemProperty -Path $RunKey -Name $AppName -Value "`"$InstalledExe`""
}

Step "Abrindo o $AppName"
Start-Process $InstalledExe
Write-Host "Pronto! O $AppName esta na bandeja." -ForegroundColor Green
