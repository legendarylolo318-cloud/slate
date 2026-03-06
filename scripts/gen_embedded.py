#!/usr/bin/env python3
"""Generate XOR-obfuscated Rust byte arrays for embedded provider configs.

Usage:
    # Default (Tidal) — backward compat with gen_credentials.py:
    python3 scripts/gen_embedded.py <key_a> <key_b>

    # Named provider:
    python3 scripts/gen_embedded.py --provider lastfm <key_a> <key_b>
    python3 scripts/gen_embedded.py --provider librefm <key_a> <key_b>

Outputs a complete embedded_<provider>.rs file to stdout.
Redirect to overwrite the module:
    python3 scripts/gen_embedded.py --provider lastfm K S > src-tauri/src/embedded_lastfm.rs
"""

import os
import sys

# Provider name -> (salt_a, hint_a, salt_b, hint_b) constant names
PROVIDER_CONSTANTS = {
    "config": ("STREAM_SALT_A", "CODEC_HINT_A", "STREAM_SALT_B", "CODEC_HINT_B"),
    "lastfm": ("SIGNAL_A", "FILTER_A", "SIGNAL_B", "FILTER_B"),
    "librefm": ("CARRIER_A", "MASK_A", "CARRIER_B", "MASK_B"),
}


def xor_encode(data: bytes, key: bytes) -> bytes:
    return bytes(a ^ b for a, b in zip(data, key))


def fmt_bytes(bs: bytes) -> str:
    """Format bytes as a Rust byte-array literal, 12 values per line."""
    lines = []
    for i in range(0, len(bs), 12):
        chunk = bs[i : i + 12]
        lines.append("    " + ", ".join(f"0x{b:02x}" for b in chunk) + ",")
    return "\n".join(lines)


def generate(provider: str, val_a_str: str, val_b_str: str) -> str:
    if provider not in PROVIDER_CONSTANTS:
        print(
            f"Error: unknown provider '{provider}'. "
            f"Supported: {', '.join(PROVIDER_CONSTANTS)}",
            file=sys.stderr,
        )
        sys.exit(1)

    salt_a, hint_a, salt_b, hint_b = PROVIDER_CONSTANTS[provider]

    val_a = val_a_str.encode()
    val_b = val_b_str.encode()

    key_a = os.urandom(len(val_a))
    key_b = os.urandom(len(val_b))

    enc_a = xor_encode(val_a, key_a)
    enc_b = xor_encode(val_b, key_b)

    return f"""\
/// Auto-generated — do not edit by hand.

const {salt_a}: [u8; {len(enc_a)}] = [
{fmt_bytes(enc_a)}
];

const {hint_a}: [u8; {len(key_a)}] = [
{fmt_bytes(key_a)}
];

const {salt_b}: [u8; {len(enc_b)}] = [
{fmt_bytes(enc_b)}
];

const {hint_b}: [u8; {len(key_b)}] = [
{fmt_bytes(key_b)}
];

fn decode(data: &[u8], mask: &[u8]) -> String {{
    data.iter()
        .zip(mask.iter())
        .map(|(d, m)| (d ^ m) as char)
        .collect()
}}

pub fn stream_key_a() -> String {{
    decode(&{salt_a}, &{hint_a})
}}

pub fn stream_key_b() -> String {{
    decode(&{salt_b}, &{hint_b})
}}

pub fn has_stream_keys() -> bool {{
    let v = stream_key_a();
    !v.is_empty() && !v.starts_with("PLACEHOLDER")
}}"""


def main():
    args = sys.argv[1:]
    provider = "config"

    if len(args) >= 2 and args[0] == "--provider":
        provider = args[1]
        args = args[2:]

    if len(args) != 2:
        print(
            f"Usage: {sys.argv[0]} [--provider <name>] <key_a> <key_b>",
            file=sys.stderr,
        )
        sys.exit(1)

    print(generate(provider, args[0], args[1]))


if __name__ == "__main__":
    main()
