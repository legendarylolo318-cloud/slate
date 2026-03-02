#!/usr/bin/env python3
"""Generate XOR-obfuscated Rust byte arrays for embedded stream config.

Usage:
    python3 scripts/gen_credentials.py <key_a> <key_b>

Outputs a complete embedded_config.rs file to stdout.
Redirect to overwrite the module:
    python3 scripts/gen_credentials.py KEY_A KEY_B > src-tauri/src/embedded_config.rs
"""

import os
import sys


def xor_encode(data: bytes, key: bytes) -> bytes:
    return bytes(a ^ b for a, b in zip(data, key))


def fmt_bytes(bs: bytes) -> str:
    """Format bytes as a Rust byte-array literal, 12 values per line."""
    lines = []
    for i in range(0, len(bs), 12):
        chunk = bs[i : i + 12]
        lines.append("    " + ", ".join(f"0x{b:02x}" for b in chunk) + ",")
    return "\n".join(lines)


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <key_a> <key_b>", file=sys.stderr)
        sys.exit(1)

    val_a = sys.argv[1].encode()
    val_b = sys.argv[2].encode()

    key_a = os.urandom(len(val_a))
    key_b = os.urandom(len(val_b))

    enc_a = xor_encode(val_a, key_a)
    enc_b = xor_encode(val_b, key_b)

    print(f"""\
/// Auto-generated — do not edit by hand.

const STREAM_SALT_A: [u8; {len(enc_a)}] = [
{fmt_bytes(enc_a)}
];

const CODEC_HINT_A: [u8; {len(key_a)}] = [
{fmt_bytes(key_a)}
];

const STREAM_SALT_B: [u8; {len(enc_b)}] = [
{fmt_bytes(enc_b)}
];

const CODEC_HINT_B: [u8; {len(key_b)}] = [
{fmt_bytes(key_b)}
];

fn decode(data: &[u8], mask: &[u8]) -> String {{
    data.iter()
        .zip(mask.iter())
        .map(|(d, m)| (d ^ m) as char)
        .collect()
}}

pub fn stream_key_a() -> String {{
    decode(&STREAM_SALT_A, &CODEC_HINT_A)
}}

pub fn stream_key_b() -> String {{
    decode(&STREAM_SALT_B, &CODEC_HINT_B)
}}

pub fn has_stream_keys() -> bool {{
    let v = stream_key_a();
    !v.is_empty() && !v.starts_with("PLACEHOLDER")
}}\
""")


if __name__ == "__main__":
    main()
