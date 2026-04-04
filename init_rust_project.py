"""一键创建 claude-code-rs Rust 项目目录结构"""
import os

base = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'claude-code-rs')
crates = ['claude-core', 'claude-api', 'claude-tools', 'claude-agent', 'claude-cli']

for crate in crates:
    path = os.path.join(base, 'crates', crate, 'src')
    os.makedirs(path, exist_ok=True)
    print(f"Created: {path}")

print("\nDone! Directory structure created.")
print("Now running: cargo --version")
os.system("cargo --version")
