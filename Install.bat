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

echo ==^> Baixando o Desjanjador (ultima versao)...
powershell -NoProfile -Command "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; Invoke-WebRequest '%EXEURL%' -OutFile '%EXE%'; try{Unblock-File '%EXE%'}catch{}"
if not exist "%EXE%" (
    echo Falha no download. Verifique a internet e tente de novo.
    pause
    exit /b 1
)

echo ==^> Abrindo o Desjanjador (vai pedir admin)...
start "" "%EXE%"
echo.
echo Pronto! Ligue "Iniciar com o Windows" dentro do app.
timeout /t 3 >nul
