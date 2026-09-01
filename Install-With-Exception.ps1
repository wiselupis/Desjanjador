<#
  Desjanjador - Instalar com excecao de antivirus/firewall.

  Same install as Install.bat (baixa a ultima versao, valida, instala e abre),
  MAS antes adiciona, de forma transparente e com o SEU consentimento (voce rodou
  este script como admin):
    1) uma EXCECAO no Windows Defender para a pasta do app, e
    2) regras de FIREWALL (entrada+saida) liberando o executavel.

  Por que existe: o Desjanjador nao e assinado e faz coisas que os antivirus
  acham suspeitas (roteia por proxies, mexe no PAC do sistema, se eleva). Isso faz
  o Defender/antivirus as vezes bloquear ou colocar o .exe em quarentena (os error
  225 / 10013). Rodar ESTE instalador resolve isso de uma vez, no ato da instalacao.

  Observacao de seguranca: excluir a pasta do Defender significa que nada dentro
  dela sera escaneado. E a propria pasta do app (%LOCALAPPDATA%\Desjanjador), e
  voce esta autorizando isso explicitamente ao rodar este script.

  So mexe no Windows Defender. Se voce usa outro antivirus (ex: BitDefender), ele
  avisa para adicionar a pasta manualmente nas excecoes de TODOS os modulos.
#>
[CmdletBinding()]
param()

$Repo   = 'wiselupis/Desjanjador'
$AppDir = Join-Path $env:LOCALAPPDATA 'Desjanjador'
$Exe    = Join-Path $AppDir 'desjanjador.exe'
$Tmp    = Join-Path $AppDir 'desjanjador.new.exe'
$ExeUrl = "https://github.com/$Repo/releases/latest/download/desjanjador.exe"

# --- 0) precisa de admin (excecao do Defender + firewall). Se nao for, re-eleva. ---
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Write-Host '==> Pedindo privilegios de administrador...'
    try {
        Start-Process -FilePath 'powershell.exe' -Verb RunAs -ArgumentList @(
            '-NoProfile','-ExecutionPolicy','Bypass','-File',"`"$PSCommandPath`"")
    } catch {
        Write-Host 'Cancelado. E preciso admin para adicionar as excecoes.'
        Read-Host 'Enter para sair'
    }
    exit
}

if (-not (Test-Path -LiteralPath $AppDir)) {
    New-Item -ItemType Directory -Path $AppDir -Force | Out-Null
}

# --- WebView2 (necessario no Windows 10) ---
Write-Host '==> Verificando WebView2...'
$wv = $false
foreach ($k in @(
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
    'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
    'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}')) {
    try { if ((Get-ItemProperty -Path $k -Name pv -ErrorAction Stop).pv) { $wv = $true; break } } catch {}
}
if (-not $wv) {
    Write-Host '    instalando WebView2 Runtime...'
    $wvExe = Join-Path $env:TEMP 'wv2.exe'
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $wvExe -UseBasicParsing
        if (Test-Path $wvExe) { Start-Process -FilePath $wvExe -ArgumentList '/silent','/install' -Wait }
    } catch { Write-Host '    (WebView2: pulei; provavelmente ja instalado)' }
}

# --- 1) excecao no Windows Defender (pasta + processo) ---
Write-Host '==> Adicionando excecao no Windows Defender...'
try {
    Add-MpPreference -ExclusionPath $AppDir -ErrorAction Stop
    Add-MpPreference -ExclusionProcess 'desjanjador.exe' -ErrorAction Stop
    Write-Host '    ok (pasta + processo).'
} catch {
    Write-Host "    nao consegui adicionar no Defender: $($_.Exception.Message)"
    Write-Host '    Se voce usa outro antivirus (ex: BitDefender), adicione ESTA pasta'
    Write-Host '    nas excecoes de TODOS os modulos (antivirus, protecao web/rede, firewall):'
    Write-Host "      $AppDir"
}

# --- 2) firewall: libera o exe (entrada + saida, todos os perfis) ---
# Mesma logica do firewall.rs: apaga qualquer regra que aponte pro exe, e re-adiciona allow.
Write-Host '==> Configurando o firewall...'
& netsh advfirewall firewall delete rule name=all "program=$Exe" 2>$null | Out-Null
foreach ($d in @('out','in')) {
    & netsh advfirewall firewall add rule name=Desjanjador dir=$d action=allow "program=$Exe" enable=yes profile=any 2>$null | Out-Null
}

# --- 3) baixa / atualiza (mesma logica do Install.bat) ---
Write-Host '==> Verificando a versao...'
try {
    $latest = ((Invoke-RestMethod -UseBasicParsing -Headers @{'User-Agent'='desjanjador'} `
        "https://api.github.com/repos/$Repo/releases/latest").tag_name) -replace '^v',''
} catch { $latest = '' }
$have = ''
if (Test-Path -LiteralPath $Exe) { try { $have = (Get-Item -LiteralPath $Exe).VersionInfo.FileVersion } catch {} }
function To3 ($v) { if (-not $v) { return '' }; (($v -split '[.,]') + @('0','0','0'))[0..2] -join '.' }
$hv = To3 $have; $lv = To3 $latest

if ($have -and ((($latest) -and ($hv -eq $lv)) -or (-not $latest))) {
    if ($latest) { Write-Host "    ja atualizado (v$have) - abrindo" }
    else { Write-Host "    sem conexao ao GitHub; abrindo o instalado (v$have)" }
} else {
    Write-Host ('    baixando ' + $(if ($latest) { "v$latest" } else { 'ultima versao' }) + '...')
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -UseBasicParsing $ExeUrl -OutFile $Tmp -ErrorAction Stop
    } catch { Write-Host '    falha no download'; Read-Host 'Enter para sair'; exit 2 }
    $ok = $false
    try { $fi = Get-Item -LiteralPath $Tmp; if ($fi.Length -gt 1000000 -and $fi.VersionInfo.FileVersion) { $ok = $true } } catch {}
    if (-not $ok) {
        Remove-Item -LiteralPath $Tmp -Force -ErrorAction SilentlyContinue
        Write-Host '    download invalido'; Read-Host 'Enter para sair'; exit 3
    }
    try { Unblock-File -LiteralPath $Tmp } catch {}
    try { Move-Item -LiteralPath $Tmp -Destination $Exe -Force -ErrorAction Stop }
    catch {
        Remove-Item -LiteralPath $Tmp -Force -ErrorAction SilentlyContinue
        Write-Host '    nao foi possivel substituir - feche o app e rode de novo'
        Read-Host 'Enter para sair'; exit 4
    }
}

if (-not (Test-Path -LiteralPath $Exe)) {
    Write-Host 'Falha ao instalar. Verifique a internet e tente de novo.'
    Read-Host 'Enter para sair'; exit 1
}

Write-Host '==> Abrindo o Desjanjador...'
Start-Process -FilePath $Exe
Write-Host ''
Write-Host 'Pronto! Excecoes aplicadas e app aberto.'
Read-Host 'Enter para fechar'
