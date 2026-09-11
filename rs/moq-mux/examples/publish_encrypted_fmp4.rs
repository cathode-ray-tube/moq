use std::{
    env,
    io::{self, Write},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use ed25519_dalek::SigningKey;
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::watch,
};

use moq_secure::key_store::{InMemoryKeyStore, KeyStore};

use moq_mux::{
    encryption::moq_secure_adapter::MoqSecureEncrypter,
    fmp4::Import,
};

const READ_BUFFER_SIZE: usize = 64 * 1024;
const DEFAULT_KEY_ID: u8 = 0;

#[derive(Debug)]
struct Options {
    input: PathBuf,
    relay_url: String,
    broadcast_name: String,
    track_name: String,
    raw: bool,
}

impl Options {
    fn parse() -> Result<Self> {
        let mut args = env::args().skip(1);

        let input = args
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("usage: debug_moq_secure <input.mp4> [options]"))?;

        let mut relay_url = "relay.example.com".to_owned();
        let mut broadcast_name = "my-stream.hang".to_owned();
        let mut track_name = "video".to_owned();
        let mut raw = false;

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--raw" => {
                    raw = true;
                }

                "--relay" => {
                    relay_url = args
                        .next()
                        .ok_or_else(|| anyhow!("--relay requires a URL"))?;
                }

                "--broadcast" => {
                    broadcast_name = args
                        .next()
                        .ok_or_else(|| anyhow!("--broadcast requires a name"))?;
                }

                "--track" => {
                    track_name = args
                        .next()
                        .ok_or_else(|| anyhow!("--track requires a track name"))?;
                }

                "-h" | "--help" => {
                    println!(
                        "usage: debug_moq_secure <input.mp4> [options]\n\
                         \n\
                         options:\n\
                           --relay <url>       relay URL\n\
                           --broadcast <name>  broadcast name\n\
                           --track <name>       raw track to subscribe to\n\
                           --raw               write binary encrypted frames\n\
                         \n\
                         environment:\n\
                           MOQ_AEAD_KEY        32-byte hex or base64 AEAD key\n\
                           MOQ_SIGNING_KEY     32-byte hex or base64 Ed25519 seed\n"
                    );

                    std::process::exit(0);
                }

                other => {
                    return Err(anyhow!("unknown argument: {other}"));
                }
            }
        }

        Ok(Self {
            input,
            relay_url,
            broadcast_name,
            track_name,
            raw,
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = Options::parse()?;

    let key_id = DEFAULT_KEY_ID;

    let mut key_store_impl = InMemoryKeyStore::empty();

    let encoded_aead_key = env::var("MOQ_AEAD_KEY").context(
        "MOQ_AEAD_KEY must contain a 32-byte key encoded as hex or base64",
    )?;

    key_store_impl
        .set_key_encoded(key_id, &encoded_aead_key)
        .context("loading MOQ_AEAD_KEY")?;

    let key_store: Arc<dyn KeyStore> = Arc::new(key_store_impl);

    let signing_key = match env::var("MOQ_SIGNING_KEY") {
        Ok(encoded) => {
            let bytes =
                decode_32_bytes(&encoded).context("decoding MOQ_SIGNING_KEY")?;

            SigningKey::from_bytes(&bytes)
        }

        // Deterministic debug-only key.
        Err(_) => SigningKey::from_bytes(&[0x42; 32]),
    };

    let encrypter = MoqSecureEncrypter::new(
        key_store,
        signing_key,
        key_id,
        0,     // n_signed
        false, // maybe_sign
        0,     // pad_len
        0,     // initial_ctr
    );

    let session = connect_to_relay(&options.relay_url)
        .await
        .context("connecting to relay")?;

    let origin = session.origin.clone();

    let (publisher_done_tx, publisher_done_rx) = watch::channel(false);

    let subscriber_name = options.broadcast_name.clone();
    let subscriber_track = options.track_name.clone();
    let subscriber_raw = options.raw;
    let subscriber_origin = origin.clone();

    let subscriber_task = tokio::spawn(async move {
        run_subscriber(
            subscriber_origin,
            subscriber_name,
            subscriber_track,
            subscriber_raw,
            publisher_done_rx,
        )
        .await
    });

    let publisher_result = run_publisher(
        &origin,
        encrypter,
        &options.input,
        &options.broadcast_name,
    )
    .await;

    let _ = publisher_done_tx.send(true);

    publisher_result?;

    subscriber_task
        .await
        .context("subscriber task panicked")??;

    // The connection task tracks the relay connection for the lifetime of
    // the publisher and subscriber.
    session.connection_task.abort();

    Ok(())
}

// -------------------------------------------------------------------------
// MoQ connection
// -------------------------------------------------------------------------

struct MoqSession {
    origin: moq_net::origin::Producer,
    connection_task: tokio::task::JoinHandle<Result<()>>,
}

async fn connect_to_relay(relay_url: &str) -> Result<MoqSession> {
    let origin = moq_net::Origin::random().produce();

    let url = url::Url::parse(relay_url)
        .context("parsing relay URL")?;

    let quic = moq_tokio::quic::Config::default();
    let config = moq_tokio::connect::Config::default();

    let client = config
        .init(quic)
        .context("initializing MoQ client")?
        .with_publisher(origin.consume())
        .with_subscriber(origin.clone());

    let reconnect = client.connect(url);

    let connection_task = tokio::spawn(async move {
        reconnect
            .closed()
            .await
            .context("MoQ connection closed")
    });

    Ok(MoqSession {
        origin,
        connection_task,
    })
}

// -------------------------------------------------------------------------
// Publisher
// -------------------------------------------------------------------------

async fn run_publisher(
    origin: &moq_net::origin::Producer,
    encrypter: MoqSecureEncrypter,
    input: &PathBuf,
    broadcast_name: &str,
) -> Result<()> {
    let broadcast = origin
        .create_broadcast(broadcast_name)
        .context("creating broadcast")?;

    let mut importer = Import::new(broadcast)
        .with_encrypter(encrypter);

    let mut ffmpeg = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .arg("-i")
        .arg(input)
        .arg("-f")
        .arg("mp4")
        .arg("-movflags")
        .arg("cmaf")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("starting ffmpeg")?;

    let mut stdout = ffmpeg
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ffmpeg stdout was not captured"))?;

    let mut buffer = vec![0u8; READ_BUFFER_SIZE];

    loop {
        let count = stdout
            .read(&mut buffer)
            .await
            .context("reading fMP4 from ffmpeg")?;

        if count == 0 {
            break;
        }

        importer
            .decode(&buffer[..count])
            .context("decoding fMP4 fragment")?;
    }

    let status = ffmpeg
        .wait()
        .await
        .context("waiting for ffmpeg")?;

    if !status.success() {
        return Err(anyhow!("ffmpeg exited with status {status}"));
    }

    importer
        .finish()
        .context("finishing fMP4 importer")?;

    Ok(())
}

// -------------------------------------------------------------------------
// Subscriber
// -------------------------------------------------------------------------

async fn run_subscriber(
    origin: moq_net::origin::Producer,
    broadcast_name: String,
    track_name: String,
    raw: bool,
    mut publisher_done: watch::Receiver<bool>,
) -> Result<()> {
    let consumer = origin.consume();

    consumer
        .routed(&broadcast_name)
        .await
        .ok_or_else(|| {
            anyhow!(
                "origin closed before broadcast `{broadcast_name}` was announced"
            )
        })?;

    let path: moq_net::Path<'_> = broadcast_name.as_str().into();

    let mut origin = origin
        .scope(&[path])
        .context("scoping subscriber origin")?
        .consume()
        .announced();

    loop {
        tokio::select! {
            update = origin.next() => {
                let Some(moq_net::announce::Update { path, broadcast }) = update else {
                    return Err(anyhow!("subscriber origin closed"));
                };

                match broadcast {
                    Some(broadcast) => {
                        let track = broadcast
                            .track(&track_name)
                            .with_context(|| {
                                format!(
                                    "locating track `{track_name}` in broadcast `{path}`"
                                )
                            })?
                            .subscribe(None)
                            .await
                            .with_context(|| {
                                format!(
                                    "subscribing to track `{track_name}`"
                                )
                            })?;

                        consume_raw_track(
                            track,
                            raw,
                            &mut publisher_done,
                        )
                        .await?;

                        return Ok(());
                    }

                    None => {
                        // The broadcast was withdrawn. Continue waiting for
                        // another announce.
                        continue;
                    }
                }
            }

            result = publisher_done.changed() => {
                result.context("waiting for publisher completion")?;

                if *publisher_done.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn consume_raw_track(
    mut track: moq_net::track::Subscriber,
    raw: bool,
    publisher_done: &mut watch::Receiver<bool>,
) -> Result<()> {
    loop {
        let group = tokio::select! {
            result = track.recv_group() => {
                result.context("receiving MoQ group")?
            }

            result = publisher_done.changed() => {
                result.context("waiting for publisher completion")?;

                if *publisher_done.borrow() {
                    return Ok(());
                }

                continue;
            }
        };

        let Some(mut group) = group else {
            return Ok(());
        };

        while let Some(frame) = group
            .read_frame()
            .await
            .context("reading raw MoQ frame")?
        {
            // frame.payload is the exact payload received from the relay.
            // No decryption, parsing, remuxing, or validation occurs here.
            print_encrypted_frame(&frame.payload, raw)?;
        }
    }
}

// -------------------------------------------------------------------------
// Key decoding and output
// -------------------------------------------------------------------------

fn decode_32_bytes(value: &str) -> Result<[u8; 32]> {
    let value = value.trim();

    if value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes =
            hex::decode(value).context("decoding hexadecimal key")?;

        return bytes
            .try_into()
            .map_err(|_| anyhow!("hex key must decode to exactly 32 bytes"));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .or_else(|_| {
            base64::engine::general_purpose::STANDARD_NO_PAD.decode(value)
        })
        .context("decoding base64 key")?;

    bytes
        .try_into()
        .map_err(|_| anyhow!("base64 key must decode to exactly 32 bytes"))
}

fn print_encrypted_frame(payload: &[u8], raw: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    if raw {
        stdout
            .write_all(payload)
            .context("writing raw encrypted frame")?;

        stdout.flush()?;
        return Ok(());
    }

    write!(
        stdout,
        "encrypted_frame len={} hex=",
        payload.len()
    )?;

    for byte in payload {
        write!(stdout, "{byte:02x}")?;
    }

    writeln!(stdout)?;
    stdout.flush()?;

    Ok(())
}
