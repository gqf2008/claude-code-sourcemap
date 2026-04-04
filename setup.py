import os
import sys
import subprocess
from pathlib import Path

base = r"E:\Users\gxh\Documents\GitHub\claude-code-sourcemap"
dirs = [
    "claude-code-rs",
    "claude-code-rs\crates\claude-core\src",
    "claude-code-rs\crates\claude-api\src",
    "claude-code-rs\crates\claude-tools\src",
    "claude-code-rs\crates\claude-agent\src",
    "claude-code-rs\crates\claude-cli\src",
]

# Change to base directory
os.chdir(base)

print(f"Working directory: {os.getcwd()}\n")
print("=" * 60)
print("Creating directories...")
print("=" * 60)

for dir_path in dirs:
    full_path = os.path.join(base, dir_path)
    try:
        Path(full_path).mkdir(parents=True, exist_ok=True)
        status = "✓" if Path(full_path).exists() else "✗"
        print(f"{status} {full_path}")
    except Exception as e:
        print(f"✗ {full_path} - Error: {e}")

print("\n" + "=" * 60)
print("Directory structure created!")
print("=" * 60)

# Verify structure
print("\nVerifying structure:")
root = os.path.join(base, "claude-code-rs")
if os.path.exists(root):
    for dirpath, dirnames, filenames in os.walk(root):
        level = dirpath.replace(root, "").count(os.sep)
        indent = " " * 2 * level
        print(f"{indent}└─ {os.path.basename(dirpath)}/")

# Check tools
print("\n" + "=" * 60)
print("Checking installed tools...")
print("=" * 60)

tools = ["python", "node", "cargo", "git"]
for tool in tools:
    try:
        result = subprocess.run([tool, "--version"], capture_output=True, text=True, timeout=5)
        if result.returncode == 0:
            version_line = result.stdout.strip().split('\n')[0]
            print(f"✓ {tool:10} → {version_line}")
        else:
            print(f"✗ {tool:10} → Not found or error")
    except FileNotFoundError:
        print(f"✗ {tool:10} → Not installed")
    except Exception as e:
        print(f"✗ {tool:10} → Error: {e}")

print("\n" + "=" * 60)
print("Setup complete!")
print("=" * 60)
