#!/usr/bin/env python3
"""Create the required directory structure for claude-code-rs"""

import os
import sys
from pathlib import Path

base_path = r"E:\Users\gxh\Documents\GitHub\claude-code-sourcemap"
directories = [
    r"claude-code-rs",
    r"claude-code-rs\crates\claude-core\src",
    r"claude-code-rs\crates\claude-api\src",
    r"claude-code-rs\crates\claude-tools\src",
    r"claude-code-rs\crates\claude-agent\src",
    r"claude-code-rs\crates\claude-cli\src",
]

try:
    os.chdir(base_path)
    print(f"Changed to: {os.getcwd()}")
    
    for directory in directories:
        full_path = os.path.join(base_path, directory)
        Path(full_path).mkdir(parents=True, exist_ok=True)
        print(f"✓ Created: {full_path}")
    
    print("\nAll directories created successfully!")
    
    # Verify the structure
    print("\n--- Verification ---")
    for root, dirs, files in os.walk(os.path.join(base_path, "claude-code-rs")):
        level = root.replace(os.path.join(base_path, "claude-code-rs"), "").count(os.sep)
        indent = " " * 2 * level
        print(f"{indent}{os.path.basename(root)}/")
        subindent = " " * 2 * (level + 1)
        for d in dirs:
            print(f"{subindent}{d}/")
    
except Exception as e:
    print(f"Error: {e}", file=sys.stderr)
    sys.exit(1)
