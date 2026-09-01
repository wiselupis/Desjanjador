@echo off
REM Lanca o Install-With-Exception.ps1 sem esbarrar na ExecutionPolicy do PowerShell.
REM O .ps1 pede admin sozinho (para a excecao do Defender + firewall).
title Desjanjador - Instalar com excecao
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0Install-With-Exception.ps1"
