# encrypted_stdout

A small demonstration of frame encryption with `MoqSecureEncrypter`.

The example:

- Creates an in-memory encryption key store.
- Encrypts two sample frames.
- Assigns each frame a unique counter (`ctr`) value for nonce construction.
- Writes the encrypted frames as hexadecimal to standard output.

This example does not use the network. It only demonstrates local frame encryption and output.

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
