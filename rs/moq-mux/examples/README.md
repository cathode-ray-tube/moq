- [Encrypted stdout](#encrypted_stdout)
- [Publish encrypted fMP4](#publish_encrypted_fmp4)

<a id="encrypted_stdout"></a>

# encrypted_stdout

A small demonstration of frame encryption with `MoqSecureEncrypter`.

The example:

- Creates an in-memory encryption key store.
- Encrypts two sample frames.
- Assigns each frame a unique encryption counter (`ctr`) for nonce construction.
- Uses ChaCha20-Poly1305 encryption.
- Writes the encrypted frames as hexadecimal to standard output.

**This example does not use the network. It only demonstrates local frame encryption and output.**

## Running

From the repository root, run:

```bash
cargo run -p moq-mux --example encrypted_stdout
```

The output includes the frame sequence number, timestamp, encrypted payload length, and encrypted payload in hexadecimal form.

## Production limitation

This is a demonstration only. The example uses an in-memory key store and starts the encryption counter at a fixed value. It does not persist encryption state or coordinate counters across processes.

For production use, never reuse the same encryption key with a previously used `ctr` value. On restart, either:

- use a new encryption key, or
- restore a securely stored counter value and continue from it.

**Reusing the same key and counter can reuse a nonce and compromise encryption security.**

<a id="publish_encrypted_fmp4"></a>

# publish_encrypted_fmp4

A small debugging application that:

1. Reads an input MP4 file using `ffmpeg`.
2. Encrypts the CMAF/fMP4 data as MoQ frame payloads.
3. Publishes the encrypted frames to a MoQ relay.
4. Subscribes directly to a named track.
5. Prints the received encrypted frame payloads.
6. Optionally writes the received encrypted payloads directly to a binary file.

The application supports:

- ChaCha20-Poly1305 encryption.
- AES-256-GCM encryption.
- A configurable initial encryption counter.
- Optional Ed25519 frame signing.
- Hexadecimal preview output.
- Raw binary output.

**This application is intended for testing and debugging only. It does not implement production security, secure key handling, authentication, authorization, access control, or hardened error handling. Do not use it with production media, credentials, or sensitive keys.**

## Requirements

- Rust and Cargo
- `ffmpeg` available on `PATH`
- A compatible MoQ relay
- An MP4 file
- A track name created by the configured `Import` implementation
- A 32-byte AEAD encryption key

## Configuration

### AEAD encryption key

The application requires an AEAD key through `MOQ_AEAD_KEY`.

The key must be exactly 32 bytes, encoded as one of:

- 64 hexadecimal characters
- Standard Base64
- Unpadded Base64

Example hexadecimal key:

```text
000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
```

The key is loaded into an in-memory `InMemoryKeyStore` under key ID `0`.

`MOQ_AEAD_KEY` is required for every invocation of `publish_encrypted_fmp4`.

### Ed25519 signing key

An optional Ed25519 signing seed can be provided through `MOQ_SIGNING_KEY`.

It must be exactly 32 bytes and use one of the supported encodings:

- 64 hexadecimal characters
- Standard Base64
- Unpadded Base64

If `MOQ_SIGNING_KEY` is omitted, the application uses the deterministic debug-only seed consisting of 32 bytes with the value `0x42`.

The default example configuration disables frame signatures by passing `maybe_sign = false` to `MoqSecureEncrypter`. Therefore, `MOQ_SIGNING_KEY` is loaded for adapter compatibility and future signing configuration, but it does not cause frames to be signed unless the publisher configuration enables signing.

## Encryption algorithms

The adapter uses the following wire-level algorithm identifiers:

| Algorithm | Wire ID | Command-line value |
|---|---:|---|
| ChaCha20-Poly1305 | `1` | `chacha20-poly1305` |
| AES-256-GCM | `2` | `aes-256-gcm` |

ChaCha20-Poly1305 is the default.

Examples:

```bash
--encryption chacha20-poly1305
```

```bash
--encryption aes-256-gcm
```

The selected algorithm must also be supported by the receiving/decrypting implementation. The algorithm identifier is written into each encrypted frame by `moq-secure`.

## Encryption counter

The application starts the adapter encryption counter at `0` by default.

Use `--counter` to select a different initial counter:

```bash
--counter 1000
```

The adapter increments its independent encryption counter for every encrypted frame. The fMP4 or MoQ sequence number is not used as the encryption counter.

Never reuse the same encryption key with a previously used counter value. If the process restarts with the same AEAD key, restore a counter value that has never previously been used, or use a new AEAD key.

## Usage

From the repository root, run the command below, replacing:

- `<32-byte-key>` with your AEAD key
- `<32-byte-seed>` with an optional Ed25519 signing seed
- `input.mp4` with your MP4 file
- `https://relay.example.com/anon` with the URL of a real [moq-relay](https://github.com/moq-dev/moq/tree/main/rs/moq-relay)

```bash
MOQ_AEAD_KEY=<32-byte-key> \
MOQ_SIGNING_KEY=<32-byte-seed> \
cargo run -p moq-mux --example publish_encrypted_fmp4 \
  input.mp4 \
  --relay https://relay.example.com/anon \
  --broadcast stream.hang \
  --track 0.m4s
```

The `--track` value must match the track name created by the publisher. The default track name is `0.m4s`.

### ChaCha20-Poly1305 example

```bash
MOQ_AEAD_KEY=<32-byte-key> \
cargo run -p moq-mux --example publish_encrypted_fmp4 \
  input.mp4 \
  --relay https://relay.example.com/anon \
  --broadcast stream.hang \
  --track 0.m4s \
  --encryption chacha20-poly1305
```

### AES-256-GCM example

```bash
MOQ_AEAD_KEY=<32-byte-key> \
cargo run -p moq-mux --example publish_encrypted_fmp4 \
  input.mp4 \
  --relay https://relay.example.com/anon \
  --broadcast stream.hang \
  --track 0.m4s \
  --encryption aes-256-gcm
```

### AES-256-GCM with an initial counter

```bash
MOQ_AEAD_KEY=<32-byte-key> \
cargo run -p moq-mux --example publish_encrypted_fmp4 \
  input.mp4 \
  --relay https://relay.example.com/anon \
  --broadcast stream.hang \
  --track 0.m4s \
  --encryption aes-256-gcm \
  --counter 1000
```

## Command-line options

```text
publish_encrypted_fmp4 <input.mp4> [options]

--relay <url>              Relay URL
--broadcast <name>         Broadcast name
--track <name>             Raw track to subscribe to
--encryption <algorithm>   chacha20-poly1305 or aes-256-gcm
--counter <number>         Initial encryption counter
--initial-counter <number> Alias for --counter
--raw                      Write binary encrypted frames to stdout
-h, --help                 Show help
```

Default values:

```text
Relay:             https://relay.example.com
Broadcast:         stream.hang
Track:             0.m4s
Encryption:        chacha20-poly1305
Initial counter:   0
Key ID:            0
Padding length:    0
Signed frames:     disabled
```

## Output

Without `--raw`, each received frame is printed as hexadecimal text. The output contains:

- MoQ group status messages
- Publisher and `ffmpeg` progress messages
- The received frame number
- The encrypted frame length
- A hexadecimal preview of the encrypted payload

Example:

```text
received MoQ group #16
received chunk #46: 29639 bytes (2042082 bytes total)
received frame #16: 133180 bytes
encrypted_frame len=133180 hex=4d4f51530100000000000000000f0000014d6dc9671948444cabfa0376716cc9dd23b97fd61dc6b1196d85d1918234cbd8dd2e97b818e93d488a7d9dbd5e4eba4bd2cabb1a5b70973e1f170061c55d13b338ca9191242903ce47e0b132281a6b8dff064dcf733075f0cac12c4b59ecfc96e85ae03e1095257dec822779a10c9fdb1ad9425803e7611ba4cdf328a6be99b230de956aea...4de463ff436a99af0ac0dc3bb641458bd03559bcf40741a331bc2e3d495154b98717382f96a348025a28eb98b8b491f04aa27404fa4f8cf65d5896a913c10e8ea61459bf626af7d50fda6a0fab4b54bbc17e5664e2df1d5474fb22133dca76b3ae7b8415e794b2a80b8c912aee0925301be098faaf64134342195f65b2fddfb740274e315c5b3338d478b2d61e7b117ff4e55db5848d
```

The output relates to:

- `ffmpeg` input processing, such as `received chunk`
- fMP4 import and encryption
- MoQ group and frame reception
- Subscriber output, such as `encrypted_frame`

## Raw output

With `--raw`, frame payloads are written directly to standard output:

```bash
MOQ_AEAD_KEY=<32-byte-key> \
cargo run -p moq-mux --example publish_encrypted_fmp4 \
  input.mp4 \
  --relay https://relay.example.com/anon \
  --broadcast stream.hang \
  --track 0.m4s \
  --raw > encrypted-frames.bin
```

To use AES-256-GCM:

```bash
MOQ_AEAD_KEY=<32-byte-key> \
cargo run -p moq-mux --example publish_encrypted_fmp4 \
  input.mp4 \
  --relay https://relay.example.com/anon \
  --broadcast stream.hang \
  --track 0.m4s \
  --encryption aes-256-gcm \
  --raw > encrypted-frames.bin
```

The raw output contains the encrypted frame payloads exactly as received from the relay.

Diagnostic messages are written to standard error, while raw encrypted frame payloads are written to standard output. This allows the binary output to be redirected to a file without including diagnostic text.

The subscriber does not decrypt, parse, validate, remux, or export the received frames.

## Multiple tracks

If the importer creates separate video and audio tracks, run separate subscriber instances for each track, or extend the application to accept multiple `--track` values.

Examples:

```bash
--track 0.m4s
```

```bash
--track 1.m4s
```

## Security warning

This is a debug application only.

It does not provide:

- Production-grade key management
- Secure memory handling or key zeroization
- Persistent counter management
- Cross-process counter coordination
- User authentication
- Relay authentication
- Authorization
- Access control
- Replay protection
- Input validation suitable for hostile input
- Secure logging
- Protection against accidental exposure through environment variables, stdout, stderr, command-line arguments, or files
- Decryption or authentication of received subscriber frames

Do not use real production keys or sensitive media with this application. Use disposable test credentials, a test relay, and a test media file.

In particular:

- Do not reuse an AEAD key and encryption counter combination.
- Do not expose `MOQ_AEAD_KEY` or `MOQ_SIGNING_KEY` in shared shell history.
- Do not treat the deterministic fallback signing key as secure.
- Do not treat the hexadecimal frame output as authenticated or successfully decrypted merely because it was received.
