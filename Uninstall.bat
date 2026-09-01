@echo off
setlocal EnableExtensions
title Desjanjador - Desinstalar

REM precisa de admin para remover a tarefa agendada
net session >nul 2>&1
if %errorlevel% neq 0 (
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs" >nul 2>&1
    exit /b
)

set "APPDIR=%LOCALAPPDATA%\Desjanjador"
echo ==^> Removendo o Desjanjador...
taskkill /im desjanjador.exe /f >nul 2>&1
schtasks /Delete /TN Desjanjador /F >nul 2>&1
netsh advfirewall firewall delete rule name=Desjanjador >nul 2>&1
powershell -NoProfile -Command "try{Remove-MpPreference -ExclusionPath '%APPDIR%' -ErrorAction SilentlyContinue; Remove-MpPreference -ExclusionProcess 'desjanjador.exe' -ErrorAction SilentlyContinue}catch{}" >nul 2>&1
reg delete "HKCU\Software\Microsoft\Windows\CurrentVersion\Run" /v Desjanjador /f >nul 2>&1
powershell -NoProfile -Command "$k='HKCU:\Software\Microsoft\Windows\CurrentVersion\Internet Settings'; $c=(Get-ItemProperty $k -Name AutoConfigURL -ErrorAction SilentlyContinue).AutoConfigURL; if($c -like '*127.0.0.1:43110*'){Remove-ItemProperty $k -Name AutoConfigURL -ErrorAction SilentlyContinue}" >nul 2>&1
rmdir /s /q "%APPDIR%" >nul 2>&1
echo Removido.
timeout /t 3 >nul
