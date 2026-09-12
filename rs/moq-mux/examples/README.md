- [Encrypted stdout](#encrypted_stdout)
- [Publish encrypted fMP4](#publish_encrypted_fmp4)


<a id="encrypted_stdout"></a>
# encrypted_stdout

A small demonstration of frame encryption with `MoqSecureEncrypter`.

The example:

- Creates an in-memory encryption key store.
- Encrypts two sample frames.
- Assigns each frame a unique counter (`ctr`) value for nonce construction.
- Writes the encrypted frames as hexadecimal to standard output.

**This example does not use the network. It only demonstrates local frame encryption and output.**

## Running

From the repository root, run:

```bash
cargo run -p moq-mux --example encrypted_stdout
```

The output includes the frame sequence number, timestamp, encrypted payload length, and encrypted payload in hexadecimal form.

## Production Limitation

This is a demo only. The example uses an in-memory key store and starts the encryption counter at a fixed value. It does not persist encryption state or coordinate counters across processes.

For production use, never reuse the same encryption key with a previously used `ctr` value. On restart, either:

   - use a new encryption key, **or**
   - restore a securely stored counter value and continue from it.

**Reusing the same key and counter can reuse a nonce and compromise encryption security.**


<a id="publish_encrypted_fmp4"></a>
# publish_encrypted_fmp4

A small debugging application that:

1. Reads an input MP4 file using `ffmpeg`.
2. Encrypts the CMAF/fMP4 data (as MoQ Frame Payloads).
3. Publishes the resulting frames to a MoQ relay.
4. Subscribes directly to a named track.
5. Prints the received raw frame payloads (First 150 bytes...Last 150 bytes).

**This application is intended for testing and debugging only. It does not implement production security, secure key handling, authentication, authorization, access control, or hardened error handling. Do not use it with production media, credentials, or sensitive keys.**

## Requirements

- Rust and Cargo
- `ffmpeg` available on `PATH`
- A compatible MoQ relay
- A track name created by the configured `Import` implementation

## Configuration

The application requires an AEAD key through `MOQ_AEAD_KEY`.

The key must be exactly 32 bytes, encoded as either:

- 64 hexadecimal characters
- Base64
- Unpadded Base64

An optional Ed25519 signing seed can be provided through `MOQ_SIGNING_KEY`. It must also be exactly 32 bytes and use one of the encodings above.

If `MOQ_SIGNING_KEY` is omitted, a deterministic debug-only signing key is used.

## Usage

```bash
MOQ_AEAD_KEY=<32-byte-key> \\
MOQ_SIGNING_KEY=<32-byte-seed> \\
cargo run -- \\
  input.mp4 \\
  --relay https://relay.example.com/anon \\
  --broadcast stream.hang \\
  --track 0.m4s
```

The `--track` value must match the track name created by the publisher. The default track name is `0.m4s`.

## Command-line options

```text
publish_encrypted_fmp4 <input.mp4> [options]

--relay <url>       Relay URL
--broadcast <name>  Broadcast name
--track <name>      Raw track to subscribe to
--raw               Write binary encrypted frames to stdout
-h, --help          Show help
```
Default values:

```text
Relay:     relay.example.com
Broadcast: stream.hang
Track:     0.m4s
```
## Output

Without `--raw`, each received frame is printed as hexadecimal text. Example output:

```text
received MoQ group #16
producer: protected frame written: plaintext=133143 bytes, timestamp=80041666666ns, new_group=true
received chunk #46: 29639 bytes (2042082 bytes total)
received frame #16: 133180 bytes
encrypted_frame len=133180 hex=4d4f51530100000000000000000f0000014d6dc9671948444cabfa0376716cc9dd23b97fd61dc6b1196d85d1918234cbd8dd2e97b818e93d488a7d9dbd5e4eba4bd2cabb1a5b70973e1f170061c55d13b338ca9191242903ce47e0b132281a6b8dff064dcf733075f0cac12c4b59ecfc96e85ae03e1095257dec822779a10c9fdb1ad9425803e7611ba4cdf328a6be99b230de956aea...4de463ff436a99af0ac0dc3bb641458bd03559bcf40741a331bc2e3d495154b98717382f96a348025a28eb98b8b491f04aa27404fa4f8cf65d5896a913c10e8ea61459bf626af7d50fda6a0fab4b54bbc17e5664e2df1d5474fb22133dca76b3ae7b8415e794b2a80b8c912aee0925301be098faaf64134342195f65b2fddfb740274e315c5b3338d478b2d61e7b117ff4e55db5848d
```
This relates to the ffmpeg process (`received chunk...`), encryption (`producer: protected frame written...`) and subscriber received frame payload (`encrypted_frame...`). 

With `--raw`, frame payloads are written directly to standard output:

```bash
MOQ_AEAD_KEY=<key> \\
cargo run -- \\
  input.mp4 \\
  --relay https://relay.example.com/anon \\
  --broadcast stream.hang \\
  --track 0.m4s \\
  --raw > encrypted-frames.bin
```

The raw output contains the frame payloads exactly as received from the relay. The subscriber does not decrypt, parse, validate, remux, or export the frames.

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

It **does not** provide:

- Production-grade key management
- Secure memory handling or key zeroization
- User authentication
- Relay authentication
- Authorization
- Replay protection
- Input validation suitable for hostile input
- Secure logging
- Protection against accidental exposure through command-line arguments, environment variables, stdout, or files

**Do not use real production keys or sensitive media with this application. Use disposable test credentials and a test relay.**
