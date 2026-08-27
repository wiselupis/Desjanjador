# Desjanjador - lancador portable (Windows 10 e 11).
#
# Dica: NAO de dois cliques no .ps1 (o Windows abre no bloco de notas).
# Use o "Desjanjador.bat" ao lado, ou clique-direito > "Executar com o PowerShell".
#
# O que faz: pede admin (UAC), garante o WebView2 (Win10), baixa o exe mais
# recente do GitHub (ou usa um ao lado), tira a marca da web (bypass do
# SmartScreen), exclui a pasta no Defender e abre o app (sem admin).
# A inicializacao com o Windows liga/desliga DENTRO do app.
#
#   Desjanjador.bat              baixa (se preciso) e abre
#   Desjanjador.bat -Uninstall  fecha, remove do startup e apaga a pasta local

param([switch]$Uninstall)

$ErrorActionPreference = "Stop"
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

# --- auto-elevacao (UAC) ---
$id = [Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    $a = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    if ($Uninstall) { $a += " -Uninstall" }
    try { Start-Process powershell.exe -Verb RunAs -ArgumentList $a }
    catch { Write-Host "Elevacao cancelada. Rode como Administrador." -ForegroundColor Yellow; Start-Sleep 3 }
    return
}

$Repo       = "wiselupis/Desjanjador"
$AppName    = "Desjanjador"
$InstallDir = Join-Path $env:LOCALAPPDATA $AppName
$ExePath    = Join-Path $InstallDir "desjanjador.exe"
$RunKey     = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$InetKey    = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings"
function Step($m) { Write-Host "==> $m" -ForegroundColor Cyan }

if ($Uninstall) {
    Step "Removendo o $AppName..."
    Get-Process desjanjador -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-ItemProperty -Path $RunKey -Name $AppName -ErrorAction SilentlyContinue
    try { Remove-MpPreference -ExclusionPath $InstallDir -ErrorAction SilentlyContinue } catch {}
    $cur = (Get-ItemProperty $InetKey -Name AutoConfigURL -ErrorAction SilentlyContinue).AutoConfigURL
    if ($cur -like "*127.0.0.1:43110*") { Remove-ItemProperty $InetKey -Name AutoConfigURL -ErrorAction SilentlyContinue }
    Remove-Item $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Removido." -ForegroundColor Green
    Start-Sleep 2
    return
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# Defender: exclui a pasta (bypass do bloqueio do exe nao assinado)
Step "Exclusao no Windows Defender"
try { Add-MpPreference -ExclusionPath $InstallDir -ErrorAction Stop }
catch { Write-Host "  (Defender indisponivel, seguindo)" -ForegroundColor DarkGray }

# WebView2 (Windows 10 pode nao ter; Windows 11 ja vem com)
function Test-WebView2 {
    $g = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    foreach ($p in @("HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$g",
            "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$g",
            "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$g")) {
        $pv = (Get-ItemProperty $p -ErrorAction SilentlyContinue).pv
        if ($pv -and $pv -ne "0.0.0.0") { return $true }
    }
    return $false
}
if (-not (Test-WebView2)) {
    Step "Instalando o WebView2 Runtime (Windows 10)..."
    $boot = Join-Path $env:TEMP "MicrosoftEdgeWebview2Setup.exe"
    Invoke-WebRequest "https://go.microsoft.com/fwlink/p/?LinkId=2124703" -OutFile $boot
    Start-Process $boot -ArgumentList "/silent", "/install" -Wait
}

# exe: usa o que esta ao lado do script, ou baixa a ultima release do GitHub
$local = Join-Path $PSScriptRoot "desjanjador.exe"
if (Test-Path $local) {
    Step "Usando o exe local"
    Copy-Item $local $ExePath -Force
} else {
    Step "Baixando a versao mais recente do GitHub..."
    $headers = @{ "User-Agent" = "Desjanjador" }
    $rel = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest" -Headers $headers
    $asset = $rel.assets | Where-Object { $_.name -like "*.exe" } | Select-Object -First 1
    if (-not $asset) { throw "Nenhum .exe na ultima release." }
    Invoke-WebRequest $asset.browser_download_url -OutFile $ExePath -Headers $headers
    Write-Host "  $($asset.name)" -ForegroundColor DarkGray
}

try { Unblock-File $ExePath } catch {}

# abre o app SEM admin (integridade normal), via explorer
Step "Abrindo o $AppName"
Start-Process explorer.exe -ArgumentList "`"$ExePath`""
Write-Host "Pronto! O $AppName esta na bandeja. Ligue 'Iniciar com o Windows' dentro do app." -ForegroundColor Green
Start-Sleep 2
