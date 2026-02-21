const fs = require('fs');
const path = require('path');

const localesDir = path.join(__dirname, '..', 'src', 'i18n', 'locales');

function collectKeys(obj, prefix = '') {
  const keys = [];
  for (const k of Object.keys(obj)) {
    const val = obj[k];
    const newPrefix = prefix ? `${prefix}.${k}` : k;
    if (val && typeof val === 'object' && !Array.isArray(val)) {
      keys.push(...collectKeys(val, newPrefix));
    } else {
      keys.push(newPrefix);
    }
  }
  return keys;
}

function loadLocaleFile(filePath) {
  const raw = fs.readFileSync(filePath, 'utf8');
  try {
    return JSON.parse(raw);
  } catch (e) {
    console.error(`Failed to parse ${filePath}:`, e.message);
    process.exit(2);
  }
}

function findLocaleFiles(dir) {
  return fs.readdirSync(dir).filter((f) => f.endsWith('.json'));
}

function main() {
  if (!fs.existsSync(localesDir)) {
    console.error('Locales directory not found:', localesDir);
    process.exit(1);
  }
  const files = findLocaleFiles(localesDir);
  if (files.length === 0) {
    console.error('No locale files found in', localesDir);
    process.exit(1);
  }

  const locales = {};
  for (const file of files) {
    const full = path.join(localesDir, file);
    const data = loadLocaleFile(full);
    const keys = collectKeys(data).sort();
    locales[file] = keys;
  }

  const filenames = Object.keys(locales);
  const base = locales[filenames[0]];
  let ok = true;
  for (let i = 1; i < filenames.length; i++) {
    const name = filenames[i];
    const other = locales[name];
    const missingInOther = base.filter((k) => !other.includes(k));
    const extraInOther = other.filter((k) => !base.includes(k));
    if (missingInOther.length || extraInOther.length) {
      ok = false;
      console.error(`Locale mismatch between ${filenames[0]} and ${name}:`);
      if (missingInOther.length) console.error('  Missing in', name, missingInOther.slice(0, 20));
      if (extraInOther.length) console.error('  Extra in', name, extraInOther.slice(0, 20));
    }
  }

  if (!ok) {
    console.error('i18n check failed: key mismatches detected');
    process.exit(3);
  }
  console.log('i18n check passed: all locale files have matching keys');
}

if (require.main === module) main();
