@echo off
setlocal enabledelayedexpansion
:: CodeSmith Windows installer
:: Copies codesmith.exe to %USERPROFILE%\bin

set "BIN_DIR=%USERPROFILE%\bin"
set "SCRIPT_DIR=%~dp0"

if not exist "%BIN_DIR%" mkdir "%BIN_DIR%"

echo Installing codesmith to %BIN_DIR%...

copy /Y "%SCRIPT_DIR%codesmith.exe" "%BIN_DIR%\codesmith.exe" >nul
if %ERRORLEVEL% neq 0 (
    echo ERROR: Failed to copy codesmith.exe
    exit /b 1
)

echo.
echo Done. codesmith installed to %BIN_DIR%.
echo.
echo Add %BIN_DIR% to your PATH:
echo   1. Open Start, search "environment variables"
echo   2. Click "Environment Variables..."
echo   3. Under "User variables", select "Path" and click "Edit"
echo   4. Click "New" and add: %BIN_DIR%
echo   5. Click OK, then restart your terminal
echo.
echo Or run this in an admin PowerShell:
echo   $p=[Environment]::GetEnvironmentVariable('Path','User'); if(($p -split ';') -notcontains '%BIN_DIR%'){[Environment]::SetEnvironmentVariable('Path',"$p;%BIN_DIR%",'User')}
echo.
echo Then run: codesmith
