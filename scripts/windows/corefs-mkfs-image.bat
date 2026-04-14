@echo off
setlocal
set "SCRIPT_DIR=%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%SCRIPT_DIR%corefs-mkfs-image.ps1" %*
exit /b %ERRORLEVEL%
