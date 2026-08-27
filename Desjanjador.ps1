# Desjanjador - lancador portable (Windows 10 e 11).
#
# Sem instalador: baixa o exe mais recente do GitHub (se nao houver um ao lado),
# garante o WebView2 (necessario em alguns Windows 10), tira a "marca da web"
# (bypass do aviso do SmartScreen) e abre o app. A inicializacao com o Windows
# liga/desliga DENTRO do app (toggle "Iniciar com o Windows").
#
#   .\Desjanjador.ps1              baixa (se preciso) e abre
#   .\Desjanjador.ps1 -Uninstall  fecha, remove do startup e apaga a pasta local

param([switch]$Uninstall)

$ErrorActionPreference = "Stop"
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
    $cur = (Get-ItemProperty $InetKey -Name AutoConfigURL -ErrorAction SilentlyContinue).AutoConfigURL
    if ($cur -like "*127.0.0.1:43110*") { Remove-ItemProperty $InetKey -Name AutoConfigURL -ErrorAction SilentlyContinue }
    Remove-Item $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Removido." -ForegroundColor Green
    return
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

# 1) WebView2 (Windows 10 pode nao ter pre-instalado; Windows 11 ja tem)
function Test-WebView2 {
    $id = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    foreach ($p in @("HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$id",
            "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$id",
            "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$id")) {
        $pv = (Get-ItemProperty $p -ErrorAction SilentlyContinue).pv
        if ($pv -and $pv -ne "0.0.0.0") { return $true }
    }
    return $false
}
if (-not (Test-WebView2)) {
    Step "Instalando o WebView2 Runtime (necessario no Windows 10)..."
    $boot = Join-Path $env:TEMP "MicrosoftEdgeWebview2Setup.exe"
    Invoke-WebRequest "https://go.microsoft.com/fwlink/p/?LinkId=2124703" -OutFile $boot
    Start-Process $boot -ArgumentList "/silent", "/install" -Wait
}

# 2) exe: usa o que esta ao lado do script, ou baixa a ultima release do GitHub
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

# 3) bypass do SmartScreen (tira a marca da web) e abre
try { Unblock-File $ExePath } catch {}
Step "Abrindo o $AppName"
Start-Process $ExePath
Write-Host "Pronto! O $AppName esta na bandeja. Ligue 'Iniciar com o Windows' dentro do app." -ForegroundColor Green
