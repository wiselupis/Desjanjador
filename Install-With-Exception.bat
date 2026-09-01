@echo off
setlocal EnableExtensions
title Desjanjador - Instalar com excecao (antivirus/firewall)
REM  ============================================================================
REM  Arquivo unico, igual ao Install.bat (duplo clique), mas ANTES de instalar
REM  ele adiciona, com o SEU consentimento (voce rodou como admin):
REM    1) uma EXCECAO no Windows Defender p/ a pasta do app (+ o processo), e
REM    2) regras de FIREWALL liberando o executavel (entrada + saida).
REM  Assim o antivirus nao poe o .exe em quarentena no meio da instalacao/update
REM  (os error 225 / 10013). Pode rodar quantas vezes quiser (idempotente).
REM  So mexe no Windows Defender; se voce usa outro antivirus (ex: BitDefender),
REM  ele avisa p/ adicionar a pasta manualmente nas excecoes de TODOS os modulos.
REM  ============================================================================
set "REPO=wiselupis/Desjanjador"
set "APPDIR=%LOCALAPPDATA%\Desjanjador"
set "EXE=%APPDIR%\desjanjador.exe"
set "EXEURL=https://github.com/%REPO%/releases/latest/download/desjanjador.exe"

REM --- precisa de admin (excecao do Defender + firewall). Se nao for, re-eleva. ---
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo ==^> Pedindo privilegios de administrador...
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs" >nul 2>&1
    exit /b
)

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

REM --- 1) excecao no Windows Defender (pasta + processo). Path via env p/ nao
REM        quebrar com apostrofo no perfil (ex: C:\Users\O'Brien\...). ---
echo ==^> Adicionando excecao no Windows Defender...
set "DJ_APPDIR=%APPDIR%"
powershell -NoProfile -Command "try{Add-MpPreference -ExclusionPath $env:DJ_APPDIR -ErrorAction Stop; Add-MpPreference -ExclusionProcess 'desjanjador.exe' -ErrorAction Stop; Write-Host '    ok (pasta + processo)'}catch{Write-Host ('    Defender nao aceitou: '+$_.Exception.Message); Write-Host '    Outro antivirus (ex: BitDefender)? Adicione esta pasta nas excecoes de TODOS os modulos:'; Write-Host ('      '+$env:DJ_APPDIR)}"

REM --- 2) firewall: libera o exe (entrada + saida, todos os perfis).
REM        Mesma logica do firewall.rs: apaga qualquer regra do exe e re-adiciona. ---
echo ==^> Configurando o firewall...
netsh advfirewall firewall delete rule name=all program="%EXE%" >nul 2>&1
netsh advfirewall firewall add rule name=Desjanjador dir=out action=allow program="%EXE%" enable=yes profile=any >nul 2>&1
netsh advfirewall firewall add rule name=Desjanjador dir=in  action=allow program="%EXE%" enable=yes profile=any >nul 2>&1

REM --- fecha instancia aberta: libera o .exe pro update e evita 2 janelas
REM     (a porta 127.0.0.1:43110 so aceita uma instancia). ---
taskkill /im desjanjador.exe /f >nul 2>&1

REM --- 3) baixa / atualiza (mesma logica do Install.bat) ---
echo ==^> Verificando a versao...
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

echo ==^> Abrindo o Desjanjador...
start "" "%EXE%"
echo.
echo Pronto! Excecoes aplicadas e app aberto. Pode fechar esta janela.
timeout /t 5 >nul
