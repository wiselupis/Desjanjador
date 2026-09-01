@echo off
setlocal EnableExtensions
title Desjanjador - Instalar
set "REPO=wiselupis/Desjanjador"
set "APPDIR=%LOCALAPPDATA%\Desjanjador"
set "EXE=%APPDIR%\desjanjador.exe"
set "EXEURL=https://github.com/%REPO%/releases/latest/download/desjanjador.exe"

if not exist "%APPDIR%" mkdir "%APPDIR%"

echo ==^> Verificando WebView2 (necessario no Windows 10)
set "WV="
for %%K in (
    "HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    "HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    "HKCU\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
) do reg query %%K /v pv >nul 2>&1 && set "WV=1"
if not defined WV (
    echo    instalando WebView2 Runtime...
    powershell -NoProfile -Command "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; Invoke-WebRequest 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile '%TEMP%\wv2.exe'" >nul 2>&1
    if exist "%TEMP%\wv2.exe" start /wait "" "%TEMP%\wv2.exe" /silent /install
)

echo ==^> Verificando a versao...
rem  Paths go through the environment (not interpolated into quotes), so an
rem  apostrophe in the profile path (e.g. C:\Users\O'Brien\...) can't break the
rem  PowerShell command. We download to a temp file, validate it (size + version
rem  resource), then atomically move it into place, so a partial/failed download
rem  never clobbers a working install and a corrupt exe is never launched.
set "DJ_REPO=%REPO%"
set "DJ_EXE=%EXE%"
set "DJ_URL=%EXEURL%"
set "DJ_TMP=%APPDIR%\desjanjador.new.exe"
powershell -NoProfile -ExecutionPolicy Bypass -Command "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; $repo=$env:DJ_REPO; $exe=$env:DJ_EXE; $url=$env:DJ_URL; $tmp=$env:DJ_TMP; try{$latest=((Invoke-RestMethod -UseBasicParsing -Headers @{'User-Agent'='desjanjador'} \"https://api.github.com/repos/$repo/releases/latest\").tag_name) -replace '^v',''}catch{$latest=''}; $have=''; if(Test-Path -LiteralPath $exe){try{$have=(Get-Item -LiteralPath $exe).VersionInfo.FileVersion}catch{}}; $n3={param($v) if(-not $v){''}else{(($v -split '[.,]') + @('0','0','0'))[0..2] -join '.'}}; $hv=(& $n3 $have); $lv=(& $n3 $latest); if($have -and ((($latest) -and ($hv -eq $lv)) -or (-not $latest))){ if($latest){Write-Host ('    ja atualizado (v'+$have+') - abrindo')}else{Write-Host ('    sem conexao ao GitHub; abrindo o instalado (v'+$have+')')}; exit 10 }; Write-Host ('    baixando '+$(if($latest){'v'+$latest}else{'ultima versao'})+'...'); try{Invoke-WebRequest -UseBasicParsing $url -OutFile $tmp -ErrorAction Stop}catch{Write-Host '    falha no download'; exit 2}; $ok=$false; try{$fi=Get-Item -LiteralPath $tmp; if($fi.Length -gt 1000000 -and $fi.VersionInfo.FileVersion){$ok=$true}}catch{}; if(-not $ok){Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue; Write-Host '    download invalido'; exit 3}; try{Unblock-File -LiteralPath $tmp}catch{}; try{Move-Item -LiteralPath $tmp -Destination $exe -Force -ErrorAction Stop; exit 0}catch{Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue; Write-Host '    nao foi possivel substituir - feche o app; se persistir, o antivirus pode estar bloqueando (adicione a pasta as excecoes)'; exit 4}"
if not exist "%EXE%" (
    echo Falha ao instalar o Desjanjador. Verifique a internet e tente de novo.
    pause
    exit /b 1
)

echo ==^> Abrindo o Desjanjador (vai pedir admin)...
start "" "%EXE%"
echo.
echo Pronto! Ligue "Iniciar com o Windows" dentro do app.
timeout /t 3 >nul
