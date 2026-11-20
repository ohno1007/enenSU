@echo off
setlocal
pushd "%~dp0"
if exist "%~dp0platform-tools\adb.exe" set "PATH=%~dp0platform-tools;%PATH%"
if exist "%~dp0enensu.exe" (
  "%~dp0enensu.exe" install
) else (
  "%~dp0target\release\enensu.exe" install
)
set "install_result=%errorlevel%"
pause
popd
exit /b %install_result%
