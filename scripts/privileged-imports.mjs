import { closeSync, fstatSync, openSync, readFileSync } from 'node:fs';

const systemDlls = new Set([
  'advapi32.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'combase.dll', 'crypt32.dll',
  'gdi32.dll', 'kernel32.dll', 'kernelbase.dll', 'normaliz.dll', 'ntdll.dll',
  'ole32.dll', 'oleaut32.dll', 'rpcrt4.dll', 'secur32.dll', 'shell32.dll',
  'shlwapi.dll', 'user32.dll', 'userenv.dll', 'version.dll', 'wintrust.dll', 'ws2_32.dll',
]);

// The elevated bootstrap initially runs from a user-writable package folder.
// Reject a release that would load a private CRT or another adjacent DLL before
// its Rust entry point can establish a restricted DLL search policy.
export function verifyPrivilegedImports(file) {
  const fd = openSync(file, 'r');
  let bytes;
  try {
    const info = fstatSync(fd);
    if (!info.isFile() || info.size > 64 * 1024 * 1024) throw new Error('Invalid privileged executable size');
    bytes = readFileSync(fd);
  } finally { closeSync(fd); }
  const ensure = (offset, length) => {
    if (!Number.isInteger(offset) || offset < 0 || offset + length > bytes.length) throw new Error('Invalid privileged PE bounds');
    return offset;
  };
  const u16 = (offset) => bytes.readUInt16LE(ensure(offset, 2));
  const u32 = (offset) => bytes.readUInt32LE(ensure(offset, 4));
  if (u16(0) !== 0x5a4d) throw new Error('Invalid privileged PE header');
  const pe = u32(0x3c);
  if (u32(pe) !== 0x4550 || u16(pe + 4) !== 0x8664) throw new Error('Privileged helpers must be Windows x64 executables');
  const optional = pe + 24;
  const optionalSize = u16(pe + 20);
  if (u16(optional) !== 0x20b || optionalSize < 240 || u32(optional + 108) < 14) throw new Error('Invalid privileged PE optional header');
  const count = u16(pe + 6);
  if (!count || count > 96) throw new Error('Invalid privileged PE sections');
  const sections = Array.from({ length: count }, (_, index) => {
    const section = optional + optionalSize + index * 40;
    ensure(section, 40);
    return { rva: u32(section + 12), size: u32(section + 16), raw: u32(section + 20) };
  });
  const fileOffset = (rva, length) => {
    const section = sections.find((entry) => rva >= entry.rva && rva + length <= entry.rva + entry.size);
    if (!section) throw new Error('Invalid privileged PE import address');
    return ensure(section.raw + rva - section.rva, length);
  };
  const dllName = (rva) => {
    const offset = fileOffset(rva, 1);
    const end = bytes.indexOf(0, offset);
    if (end < offset || end - offset > 260) throw new Error('Invalid privileged DLL name');
    const name = bytes.subarray(offset, end).toString('ascii').toLowerCase();
    if (!/^[a-z0-9_.-]+\.dll$/.test(name)) throw new Error('Invalid privileged DLL name');
    return name;
  };
  const imports = new Set();
  for (const [index, descriptorSize, nameOffset] of [[1, 20, 12], [13, 32, 4]]) {
    const entry = optional + 112 + index * 8;
    const rva = u32(entry);
    const size = u32(entry + 4);
    if (!rva && !size) continue;
    if (!rva || size < descriptorSize || size > 1024 * descriptorSize) throw new Error('Invalid privileged PE import table');
    let ended = false;
    for (let at = 0; at + descriptorSize <= size; at += descriptorSize) {
      const offset = fileOffset(rva + at, descriptorSize);
      if (bytes.subarray(offset, offset + descriptorSize).every((value) => value === 0)) { ended = true; break; }
      if (index === 13 && u32(offset) !== 1) throw new Error('Unsupported delayed DLL import addressing');
      imports.add(dllName(u32(offset + nameOffset)));
    }
    if (!ended) throw new Error('Unterminated privileged PE import table');
  }
  for (const name of imports) {
    if (!systemDlls.has(name) && !/^api-ms-win-(core|security|service)-[a-z0-9-]+\.dll$/.test(name)) {
      throw new Error(`Privileged helper imports a non-system DLL: ${name}`);
    }
  }
  return [...imports].sort();
}
