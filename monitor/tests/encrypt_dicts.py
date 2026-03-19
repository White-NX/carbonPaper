"""
One-time script to encrypt dictionary files with AES-256-GCM.

Reads each dict_XX.dict file from compliance_process/dicts/, encrypts it,
and writes a .dict.enc file in the format: [12-byte nonce][ciphertext + 16-byte GCM tag].

The encryption key is derived from SHA-256 of b"CarbonPaper-SensitiveDict-v1",
matching the Rust-side decryption.

Usage:
    python compliance_process/encrypt_dicts.py
"""

import hashlib
import os
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

DICT_DIR = os.path.join(os.path.dirname(__file__), "dicts")
KEY_MATERIAL = b"CarbonPaper-SensitiveDict-v1"

DICT_FILES = [
    "dict_01.dict",
    "dict_02.dict",
    "dict_03.dict",
    "dict_04.dict",
    "dict_05.dict",
]


def derive_key() -> bytes:
    return hashlib.sha256(KEY_MATERIAL).digest()


def encrypt_file(input_path: str, output_path: str, key: bytes) -> None:
    aesgcm = AESGCM(key)
    nonce = os.urandom(12)

    with open(input_path, "rb") as f:
        plaintext = f.read()

    # AESGCM.encrypt returns ciphertext + 16-byte tag appended
    ciphertext = aesgcm.encrypt(nonce, plaintext, None)

    with open(output_path, "wb") as f:
        f.write(nonce + ciphertext)

    print(f"  {os.path.basename(input_path)} -> {os.path.basename(output_path)} ({len(plaintext)} -> {len(nonce) + len(ciphertext)} bytes)")


def main():
    key = derive_key()
    print(f"Scanning {DICT_DIR} for dictionary files...")

    count = 0
    for filename in DICT_FILES:
        input_path = os.path.join(DICT_DIR, filename)
        if not os.path.exists(input_path):
            print(f"  SKIP: {filename} not found")
            continue
        output_path = input_path + ".enc"
        encrypt_file(input_path, output_path, key)
        count += 1

    print(f"\nDone. Encrypted {count} dictionary files.")


if __name__ == "__main__":
    main()
