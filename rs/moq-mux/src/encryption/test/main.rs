use moq_mux::{
    container::{MoqFrameWriter, Sframe},
    encryption::MoqSecureEncrypter,
};

let moq_writer = MoqFrameWriter { group };

let moq_secure_writer = MoqSecureEncrypter::new(
    key_store,
    signing_key,
    key_id,
    n_signed,
    maybe_sign,
    pad_len,
);

let writer = Sframe::new(
    moq_writer,
    moq_secure_writer,
);

let producer = Producer::new(track, writer);
