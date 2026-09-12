use std::{
    env,
    io::{self, Write},
    path::{Path, PathBuf},
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
    container::fmp4::Import,
    encryption::moq_secure_adapter::MoqSecureEncrypter,
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

        let input = args.next().map(PathBuf::from).ok_or_else(|| {
            anyhow!("usage: publish_encrypted_fmp4 <input.mp4> [options]")
        })?;

        let mut relay_url = "https://relay.example.com".to_owned();
        let mut broadcast_name = "stream.hang".to_owned();
        let mut track_name = "0.m4s".to_owned();
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
                        "usage: publish_encrypted_fmp4 <input.mp4> [options]\n\
                         \n\
                         options:\n\
                           --relay <url>       relay URL\n\
                           --broadcast <name>  broadcast name\n\
                           --track <name>      track name\n\
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

    eprintln!("options: {options:?}");

    let key_id = DEFAULT_KEY_ID;
    let mut key_store_impl = InMemoryKeyStore::empty();

    let encoded_aead_key = env::var("MOQ_AEAD_KEY").context(
        "MOQ_AEAD_KEY must contain a 32-byte key encoded as hex or base64",
    )?;

    key_store_impl
        .set_key_encoded(key_id, &encoded_aead_key)
        .context("loading MOQ_AEAD_KEY")?;

    eprintln!("AEAD key loaded");

    let key_store: Arc<dyn KeyStore> = Arc::new(key_store_impl);

    let signing_key = match env::var("MOQ_SIGNING_KEY") {
        Ok(encoded) => {
            eprintln!("using MOQ_SIGNING_KEY");

            let bytes =
                decode_32_bytes(&encoded).context("decoding MOQ_SIGNING_KEY")?;

            SigningKey::from_bytes(&bytes)
        }

        Err(_) => {
            eprintln!(
                "MOQ_SIGNING_KEY is not set; using deterministic debug key"
            );

            SigningKey::from_bytes(&[0x42; 32])
        }
    };

    let encrypter = MoqSecureEncrypter::new(
        key_store,
        signing_key,
        key_id,
        0,
        false,
        0,
        0,
    );

    eprintln!("connecting to relay: {}", options.relay_url);

    let session = connect_to_relay(&options.relay_url)
        .await
        .context("connecting to relay")?;

    eprintln!("relay connection initialized");

    let origin = session.origin.clone();
    let (publisher_done_tx, publisher_done_rx) = watch::channel(false);

    let publisher_origin = origin.clone();
    let publisher_input = options.input.clone();
    let publisher_broadcast = options.broadcast_name.clone();

    eprintln!("starting publisher task...");

    let publisher_task = tokio::spawn(async move {
        run_publisher(
            &publisher_origin,
            encrypter,
            &publisher_input,
            &publisher_broadcast,
        )
        .await
    });

    let subscriber_origin = origin.clone();
    let subscriber_name = options.broadcast_name.clone();
    let subscriber_track = options.track_name.clone();
    let subscriber_raw = options.raw;

    eprintln!("starting subscriber task...");

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

    let publisher_result = publisher_task
        .await
        .context("publisher task panicked")?;

    eprintln!("publisher returned: {publisher_result:?}");

    let _ = publisher_done_tx.send(true);

    if let Err(error) = publisher_result {
        eprintln!("publisher failed: {error:#}");

        subscriber_task.abort();

        return Err(error);
    }

    eprintln!("waiting for subscriber to finish");

    subscriber_task
        .await
        .context("subscriber task panicked")??;

    eprintln!("waiting for connection task");

    session
        .connection_task
        .await
        .context("connection task panicked")??;

    eprintln!("program finished successfully");

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
    let origin = moq_tokio::origin::spawn(moq_net::Hop::random());

    let url = url::Url::parse(relay_url)
        .with_context(|| format!("parsing relay URL `{relay_url}`"))?;

    eprintln!("parsed relay URL: {url}");

    let quic = moq_tokio::quic::Config::default();
    let config = moq_tokio::connect::Config::default();

    let client = config
        .init(quic)
        .context("initializing MoQ client")?
        .with_publisher(&origin)
        .with_subscriber(origin.clone());

    let reconnect = client.connect(url);

    let connection_task = tokio::spawn(async move {
        eprintln!("MoQ connection task started");

        let result = reconnect
            .closed()
            .await
            .context("MoQ connection closed");

        eprintln!("MoQ connection task ended: {result:?}");

        result
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
    input: &Path,
    broadcast_name: &str,
) -> Result<()> {
    eprintln!("creating broadcast `{broadcast_name}`");

    let mut broadcast = origin
        .create_broadcast(broadcast_name)
        .context("creating broadcast")?;

    // This is where the broadcast is announced in the newer API.
    broadcast
        .announce(Default::default())
        .context("announcing broadcast")?;

    eprintln!("broadcast created locally: `{broadcast_name}`");

    eprintln!("creating catalog");

    let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;

    let mut importer =
        Import::new(broadcast, catalog.reserve()).with_encrypter(encrypter);

    eprintln!("starting ffmpeg with input `{}`", input.display());

    let mut ffmpeg = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("warning")
        .arg("-i")
        .arg(input)
        .arg("-map")
        .arg("0:v:0")
        .arg("-an")
        .arg("-f")
        .arg("mp4")
        .arg("-movflags")
        .arg("+empty_moov+default_base_moof+frag_keyframe")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("starting ffmpeg")?;

    eprintln!("ffmpeg started");

    let mut stdout = ffmpeg
        .stdout
        .take()
        .ok_or_else(|| anyhow!("ffmpeg stdout was not captured"))?;

    let mut buffer = vec![0u8; READ_BUFFER_SIZE];
    let mut total_bytes = 0usize;
    let mut read_count = 0usize;

    loop {
        let count = stdout
            .read(&mut buffer)
            .await
            .context("reading fMP4 from ffmpeg")?;

        if count == 0 {
            break;
        }

        read_count += 1;
        total_bytes += count;

        eprintln!(
            "received chunk #{read_count}: {count} bytes ({total_bytes} bytes total)"
        );

        importer
            .decode(&buffer[..count])
            .context("decoding fMP4 fragment")?;
    }

    eprintln!(
        "ffmpeg stdout ended; received {total_bytes} bytes in {read_count} chunks"
    );

    let status = ffmpeg.wait().await.context("waiting for ffmpeg")?;

    eprintln!("ffmpeg exited with status {status}");

    if !status.success() {
        return Err(anyhow!("ffmpeg exited with status {status}"));
    }

    eprintln!("finishing fMP4 importer");

    importer.finish().context("finishing fMP4 importer")?;

    eprintln!("publisher finished successfully");

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
    eprintln!(
        "subscriber waiting for broadcast `{broadcast_name}` and track `{track_name}`"
    );

    let consumer = origin.consume();

    let broadcast = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        consumer.routed_broadcast(&broadcast_name),
    )
    .await
    .context("timed out waiting for broadcast")?
    .with_context(|| {
        format!("resolving broadcast `{broadcast_name}`")
    })?;

    eprintln!("broadcast `{broadcast_name}` resolved");
    eprintln!("looking for track `{track_name}`");

    let track = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        async {
            loop {
                if *publisher_done.borrow() {
                    anyhow::bail!(
                        "publisher finished before track `{track_name}` was created"
                    );
                }

                match broadcast.track(&track_name) {
                    Ok(track) => match track.subscribe(None).await {
                        Ok(track) => {
                            break Ok::<_, anyhow::Error>(track);
                        }

                        Err(error) => {
                            eprintln!(
                                "track `{track_name}` is not ready: {error}; retrying"
                            );
                        }
                    },

                    Err(error) => {
                        eprintln!(
                            "track `{track_name}` not found: {error}; retrying"
                        );
                    }
                }

                tokio::time::sleep(
                    std::time::Duration::from_millis(100),
                )
                .await;
            }
        },
    )
    .await
    .context("timed out waiting for track")??;

    eprintln!("subscribed to track `{track_name}`");

    consume_raw_track(track, raw, &mut publisher_done).await
}


async fn consume_raw_track(
    mut track: moq_net::track::Subscriber,
    raw: bool,
    publisher_done: &mut watch::Receiver<bool>,
) -> Result<()> {
    let mut group_count = 0usize;
    let mut frame_count = 0usize;

    eprintln!("waiting for MoQ groups");

    loop {
        let group = tokio::select! {
            result = track.recv_group() => {
                result.context("receiving MoQ group")?
            }

            result = publisher_done.changed() => {
                result.context("waiting for publisher completion")?;

                if *publisher_done.borrow() {
                    eprintln!(
                        "publisher completed; waiting for the track to close"
                    );
                }

                continue;
            }
        };

        let Some(mut group) = group else {
            eprintln!("track ended");
            return Ok(());
        };

        group_count += 1;

        eprintln!("received MoQ group #{group_count}");

        while let Some(frame) = group
            .read_frame()
            .await
            .context("reading raw MoQ frame")?
        {
            frame_count += 1;

            eprintln!(
                "received frame #{frame_count}: {} bytes",
                frame.payload.len()
            );

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
        let bytes = hex::decode(value).context("decoding hexadecimal key")?;

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

const PREVIEW_BYTES: usize = 150;

fn print_encrypted_frame(payload: &[u8], raw: bool) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    if raw {
        stdout
            .write_all(payload)
            .context("writing raw encrypted frame")?;

        stdout.flush().context("flushing raw output")?;
        return Ok(());
    }

    write!(stdout, "encrypted_frame len={} hex=", payload.len())
        .context("writing frame prefix")?;

    if payload.len() <= PREVIEW_BYTES * 2 {
        // The entire payload is short enough to print.
        for byte in payload {
            write!(stdout, "{byte:02x}")
                .context("writing frame byte")?;
        }
    } else {
        // Print the first 150 bytes.
        for byte in &payload[..PREVIEW_BYTES] {
            write!(stdout, "{byte:02x}")
                .context("writing frame prefix bytes")?;
        }

        write!(stdout, "...").context("writing frame separator")?;

        // Print the last 150 bytes.
        for byte in &payload[payload.len() - PREVIEW_BYTES..] {
            write!(stdout, "{byte:02x}")
                .context("writing frame suffix bytes")?;
        }
    }

    writeln!(stdout).context("writing frame newline")?;
    stdout.flush().context("flushing frame output")?;

    Ok(())
}

