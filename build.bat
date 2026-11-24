@echo off
rem One-command build:   build.bat [-KeepVersion] [-Version 1.2.3]
rem
rem Windows runs with script execution disabled by default, so the work is done by
rem build.ps1 in a child process that is allowed to run it. Everything Windows
rem shows is the same as if the script had been called directly.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
exit /b %ERRORLEVEL%
