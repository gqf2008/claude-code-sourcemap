@echo off
setlocal enabledelayedexpansion

cd /d "E:\Users\gxh\Documents\GitHub\claude-code-sourcemap"

if not exist "claude-code-rs" mkdir "claude-code-rs"
if not exist "claude-code-rs\crates\claude-core\src" mkdir "claude-code-rs\crates\claude-core\src"
if not exist "claude-code-rs\crates\claude-api\src" mkdir "claude-code-rs\crates\claude-api\src"
if not exist "claude-code-rs\crates\claude-tools\src" mkdir "claude-code-rs\crates\claude-tools\src"
if not exist "claude-code-rs\crates\claude-agent\src" mkdir "claude-code-rs\crates\claude-agent\src"
if not exist "claude-code-rs\crates\claude-cli\src" mkdir "claude-code-rs\crates\claude-cli\src"

echo.
echo Directory structure created successfully!
echo.
echo Directory listing:
tree /f claude-code-rs

echo.
echo Checking for Cargo:
cargo --version

echo.
echo Python check:
python --version

echo.
echo Node.js check:
node --version
