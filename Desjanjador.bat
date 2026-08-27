@echo off
REM Dois cliques aqui para abrir o Desjanjador (chama o PowerShell contornando a
REM politica de execucao). Aceita -Uninstall:  Desjanjador.bat -Uninstall
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0Desjanjador.ps1" %*
