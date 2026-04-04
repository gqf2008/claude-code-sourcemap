@echo off
REM Create Rust project directory structure
setlocal enabledelayedexpansion

set "BASE_DIR=E:\Users\gxh\Documents\GitHub\claude-code-sourcemap\claude-code-rs\crates"

echo Creating directory structure...

mkdir "%BASE_DIR%\claude-core\src" 2>nul
if exist "%BASE_DIR%\claude-core\src" (
    echo Created: %BASE_DIR%\claude-core\src
) else (
    echo Failed to create: %BASE_DIR%\claude-core\src
)

mkdir "%BASE_DIR%\claude-api\src" 2>nul
if exist "%BASE_DIR%\claude-api\src" (
    echo Created: %BASE_DIR%\claude-api\src
) else (
    echo Failed to create: %BASE_DIR%\claude-api\src
)

mkdir "%BASE_DIR%\claude-tools\src" 2>nul
if exist "%BASE_DIR%\claude-tools\src" (
    echo Created: %BASE_DIR%\claude-tools\src
) else (
    echo Failed to create: %BASE_DIR%\claude-tools\src
)

mkdir "%BASE_DIR%\claude-agent\src" 2>nul
if exist "%BASE_DIR%\claude-agent\src" (
    echo Created: %BASE_DIR%\claude-agent\src
) else (
    echo Failed to create: %BASE_DIR%\claude-agent\src
)

mkdir "%BASE_DIR%\claude-cli\src" 2>nul
if exist "%BASE_DIR%\claude-cli\src" (
    echo Created: %BASE_DIR%\claude-cli\src
) else (
    echo Failed to create: %BASE_DIR%\claude-cli\src
)

echo.
echo All directories created successfully!
pause
