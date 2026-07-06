const fs = require('fs');
const path = require('path');

const localesDir = path.join(__dirname, '..', 'src', 'i18n', 'locales');
const sourceDir = path.join(__dirname, '..', 'src');

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

function hasKey(obj, key) {
  let current = obj;
  for (const part of key.split('.')) {
    if (!current || typeof current !== 'object' || !(part in current)) {
      return false;
    }
    current = current[part];
  }
  return true;
}

function findSourceFiles(dir, result = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      findSourceFiles(fullPath, result);
      continue;
    }
    if (!/\.(js|jsx|ts|tsx)$/.test(entry.name)) continue;
    if (/\.(test|spec)\.(js|jsx|ts|tsx)$/.test(entry.name)) continue;
    result.push(fullPath);
  }
  return result;
}

function collectLiteralTranslationKeys(filePath) {
  const raw = fs.readFileSync(filePath, 'utf8');
  const keys = [];
  const regex = /\bt\(\s*(['"])([^'"`]+)\1/g;
  let match;
  while ((match = regex.exec(raw)) !== null) {
    const key = match[2];
    if (!key || key.includes('${')) continue;
    const line = raw.slice(0, match.index).split(/\r?\n/).length;
    keys.push({ key, line });
  }
  return keys;
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
  const localeData = {};
  for (const file of files) {
    const full = path.join(localesDir, file);
    const data = loadLocaleFile(full);
    localeData[file] = data;
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

  const baseData = localeData[filenames[0]];
  const missingSourceKeys = [];
  for (const filePath of findSourceFiles(sourceDir)) {
    for (const { key, line } of collectLiteralTranslationKeys(filePath)) {
      if (hasKey(baseData, key)) continue;
      missingSourceKeys.push({
        file: path.relative(path.join(__dirname, '..'), filePath),
        line,
        key,
      });
    }
  }

  if (missingSourceKeys.length) {
    ok = false;
    console.error('Missing locale keys referenced by source:');
    for (const item of missingSourceKeys.slice(0, 50)) {
      console.error(`  ${item.file}:${item.line} ${item.key}`);
    }
    if (missingSourceKeys.length > 50) {
      console.error(`  ...and ${missingSourceKeys.length - 50} more`);
    }
  }

  if (!ok) {
    console.error('i18n check failed');
    process.exit(3);
  }
  console.log('i18n check passed: locale files match and source keys exist');
}

if (require.main === module) main();
