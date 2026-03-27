@echo off
setlocal
echo %* | findstr /C:"--build" >nul
if %errorlevel%==0 (
  cmake %*
) else (
  cmake -DCMAKE_POLICY_VERSION_MINIMUM=3.5 %*
)
