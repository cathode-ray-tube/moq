use bytes::Bytes;
use ed25519_dalek::SigningKey;
use moq_mux::container::{FrameWriter, Sframe};
use moq_mux::encryption::moq_secure_adapter::MoqSecureEncrypter;
use moq_mux::Error;
use moq_secure::key_store::InMemoryKeyStore;
use std::io::{self, Write};
use std::sync::Arc;

struct StdoutWriter {
    sequence_number: u32,
}
impl FrameWriter for StdoutWriter {
    type Error = Error;

    fn write_frame(
        &mut self,
        timestamp: moq_net::Timestamp,
        payload: Bytes,
    ) -> Result<(), Self::Error> {
        let mut stdout = io::stdout().lock();

        writeln!(
            stdout,
            "sequence={} timestamp={:?} encrypted_len={}",
            self.sequence_number,
            timestamp,
            payload.len()
        )
        .map_err(|error| Error::Io(Arc::new(error)))?;

        writeln!(stdout, "{}", hex::encode(&payload))
            .map_err(|error| Error::Io(Arc::new(error)))?;

        self.sequence_number += 1;
        Ok(())
    }

    fn next_sequence_number(&self) -> u32 {
        self.sequence_number
    }
}


fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut key_store = InMemoryKeyStore::empty();

    key_store.set_key(
        1,
        [
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
            0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
            0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f,
        ],
    );

    let signing_key = SigningKey::from_bytes(&[0x42; 32]);

    let encrypter = MoqSecureEncrypter::new(
        &key_store,
        &signing_key,
        1,     // key_id
        0,     // n_signed
        false, // maybe_sign
        0,     // pad_len
    );

    let inner = StdoutWriter {
        sequence_number: 0,
    };

    let mut writer = Sframe::new(inner, encrypter);

    writer.write_frame(
        moq_net::Timestamp::from_secs(1)?,
        Bytes::from_static(b"keyframe NAL data"),
    )?;

    writer.write_frame(
        moq_net::Timestamp::from_secs(2)?,
        Bytes::from_static(b"delta frame"),
    )?;

    Ok(())
}
