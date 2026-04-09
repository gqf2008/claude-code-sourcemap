import os
import re

base = r"E:\Users\gxh\Documents\GitHub\claude-code-sourcemap\claude-code-rs\crates"

pattern = re.compile(r'\.(unwrap|expect)\(')
cfg_test = re.compile(r'#\[cfg\(test\)\]')

results = []

for root, dirs, files in os.walk(base):
    # Skip /tests/ directories
    if os.sep + 'tests' + os.sep in root + os.sep:
        continue
    for fname in files:
        if not fname.endswith('.rs'):
            continue
        # Skip files named *tests.rs
        if 'tests.rs' in fname:
            continue
        # Also skip test-specific files
        if 'e2e_tests' in fname:
            continue

        fpath = os.path.join(root, fname)
        with open(fpath, 'r', encoding='utf-8', errors='replace') as f:
            lines = f.readlines()

        cfg_line = None
        for i, line in enumerate(lines):
            if cfg_test.search(line):
                cfg_line = i
                break

        if cfg_line is not None:
            prod_lines = lines[:cfg_line]
        else:
            prod_lines = lines

        count = 0
        for line in prod_lines:
            count += len(pattern.findall(line))

        if count >= 3:
            rel = os.path.relpath(fpath, os.path.dirname(base))
            results.append((count, rel))

results.sort(key=lambda x: -x[0])
for count, path in results:
    print(f"{count} {path}")

print(f"\nTotal files with 3+ unwrap/expect in prod code: {len(results)}")
