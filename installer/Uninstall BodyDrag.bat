@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0BodyDrag-Setup.ps1" -Action uninstall %*
pause
