const fs = require('fs');
const path = require('path');

const basePath = 'E:\\Users\\gxh\\Documents\\GitHub\\claude-code-sourcemap';
const directories = [
    'claude-code-rs',
    'claude-code-rs\\crates\\claude-core\\src',
    'claude-code-rs\\crates\\claude-api\\src',
    'claude-code-rs\\crates\\claude-tools\\src',
    'claude-code-rs\\crates\\claude-agent\\src',
    'claude-code-rs\\crates\\claude-cli\\src',
];

try {
    for (const dir of directories) {
        const fullPath = path.join(basePath, dir);
        if (!fs.existsSync(fullPath)) {
            fs.mkdirSync(fullPath, { recursive: true });
            console.log(`✓ Created: ${fullPath}`);
        } else {
            console.log(`✓ Already exists: ${fullPath}`);
        }
    }
    
    console.log('\nAll directories ready!');
    
    // Check Rust/Cargo availability
    const { execSync } = require('child_process');
    try {
        const cargoVersion = execSync('cargo --version', { encoding: 'utf-8' });
        console.log(`\n${cargoVersion.trim()}`);
    } catch (e) {
        console.log('\n⚠ Cargo is not installed or not in PATH');
    }
    
} catch (error) {
    console.error(`Error: ${error.message}`);
    process.exit(1);
}
