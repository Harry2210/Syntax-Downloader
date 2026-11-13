@echo off
chcp 65001 >nul
title Syntax Downloader
cd /d "%~dp0"

echo ==============================================
echo   Syntax Downloader - Starting Application
echo ==============================================

call npm.cmd start
